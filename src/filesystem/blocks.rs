use crate::filesystem::FloconContext;
use std::io;
use std::io::Read;
use std::io::{Seek, Write};
use std::sync::Arc;

pub trait DataSource {
    /// Reads the content from the data source into the provided writer
    fn read_to(&self, context: &mut FloconContext, w: &mut dyn Write) -> io::Result<usize>;

    /// Tells you how big is this data source
    fn size(&self) -> io::Result<u64>;

    /// Performs a slicing of the original data and returns a "view" of a
    /// narrower part of it
    fn slice(&self, offset: u64, size: u64) -> io::Result<Box<dyn DataSource>>;

    /// Generates a clone. Not sure why the trait doesn't work or whatever
    /// but I'll just implement that manually for now
    fn clone(&self) -> Box<dyn DataSource>;
}

/// A data source that takes hold in the RAM
pub struct MemoryDataSource {
    /// The underlying data
    data: Arc<Vec<u8>>,

    /// Offset at which data is read
    offset: u64,

    /// How much data do we want
    size: u64,
}

impl MemoryDataSource {
    pub fn new(data: Arc<Vec<u8>>, offset: u64, size: u64) -> io::Result<Self> {
        let data_len = data.len() as u64;

        if offset > data_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Offset out of bounds",
            ));
        }

        match offset.checked_add(size) {
            Some(end) if end <= data_len => Ok(Self { data, offset, size }),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Size exceeds available data",
            )),
        }
    }

    /// Creates a memory data source from the given data, with offset and
    /// length covering 100% of the data at this point. Then it will narrow
    /// down when slice() is called.
    pub fn from_data(data: Vec<u8>) -> io::Result<Self> {
        let data_len = data.len() as u64;
        Self::new(Arc::new(data), 0, data_len)
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        // Convert to usize with proper error handling
        let start = self.offset as usize;
        let end = (self.offset + self.size) as usize;

        // This should be safe now due to constructor validation
        &self.data[start..end]
    }
}

impl DataSource for MemoryDataSource {
    /// We're generating our actual slice and feeding it into the writer
    fn read_to(&self, context: &mut FloconContext, w: &mut dyn Write) -> io::Result<usize> {
        w.write_all(self.as_slice())?;
        Ok(self.size as usize)
    }

    /// Unsurprisingly the size is the size
    fn size(&self) -> io::Result<u64> {
        Ok(self.size)
    }

    /// We're computing a new view of the data based on the slice asked. The
    /// only thing is that the slice must not exceed the original data size,
    /// obviously.
    fn slice(&self, offset: u64, size: u64) -> io::Result<Box<dyn DataSource>> {
        let new_size = size;
        let new_offset = self
            .offset
            .checked_add(offset)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Offset overflow"))?;

        if offset.checked_add(size).map_or(true, |end| end > self.size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Slice out of bounds",
            ));
        }

        MemoryDataSource::new(Arc::clone(&self.data), new_offset, new_size)
            .map(|ds| Box::new(ds) as Box<dyn DataSource>)
    }

    fn clone(&self) -> Box<dyn DataSource> {
        Box::new(
            MemoryDataSource::new(Arc::clone(&self.data), self.offset, self.size)
                .expect("Clone should never fail as the original was valid"),
        )
    }
}

pub struct ZeroDataSource {
    /// The size of zeros to return
    size: u64,
}

impl ZeroDataSource {
    pub fn new(size: u64) -> Self {
        Self { size }
    }
}

impl DataSource for ZeroDataSource {
    fn read_to(&self, context: &mut FloconContext, w: &mut dyn Write) -> io::Result<usize> {
        let zeros = vec![0u8; self.size as usize];
        w.write_all(&zeros)?;
        Ok(self.size as usize)
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.size)
    }

    fn slice(&self, offset: u64, size: u64) -> io::Result<Box<dyn DataSource>> {
        if offset.checked_add(size).map_or(true, |end| end > self.size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Slice out of bounds",
            ));
        }

        Ok(Box::new(ZeroDataSource::new(size)) as Box<dyn DataSource>)
    }

    fn clone(&self) -> Box<dyn DataSource> {
        Box::new(ZeroDataSource::new(self.size))
    }
}

pub struct SqliteDataSource {
    block_id: u64,
    offset: u64,
    size: u64,
}

impl SqliteDataSource {
    pub fn new(block_id: u64, offset: u64, size: u64) -> Self {
        Self {
            block_id,
            offset,
            size,
        }
    }
}

impl DataSource for SqliteDataSource {
    fn read_to(&self, context: &mut FloconContext, w: &mut dyn Write) -> io::Result<usize> {
        let mut blob = context
            .conn
            .blob_open(
                "main",
                "block",
                "data",
                i64::try_from(self.block_id)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
                true,
            )
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        blob.seek(io::SeekFrom::Start(self.offset))?;
        io::copy(&mut blob.take(self.size), w).map(|n| n.try_into().unwrap())
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.size)
    }

    fn slice(&self, offset: u64, size: u64) -> io::Result<Box<dyn DataSource>> {
        if offset.checked_add(size).map_or(true, |end| end > self.size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Slice out of bounds",
            ));
        }

        let new_offset = self
            .offset
            .checked_add(offset)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Offset overflow"))?;

        Ok(Box::new(SqliteDataSource {
            block_id: self.block_id,
            offset: new_offset,
            size,
        }))
    }

    fn clone(&self) -> Box<dyn DataSource> {
        Box::new(SqliteDataSource {
            block_id: self.block_id,
            offset: self.offset,
            size: self.size,
        })
    }
}

pub struct WorkingBlock {
    pub id: Option<u64>,
    pub first_byte: u64,
    pub last_byte: u64,
    pub source: Box<dyn DataSource>,
}

impl WorkingBlock {
    pub fn new(
        id: Option<u64>,
        first_byte: u64,
        last_byte: u64,
        source: Box<dyn DataSource>,
    ) -> Self {
        Self {
            id,
            first_byte,
            last_byte,
            source,
        }
    }

    /// We don't necessarily hold the actual data at the time of manipulating
    /// blocks, however at this point we want to cash out and see what we've
    /// got. The data will get written in the Write.
    pub fn concrete_data_to_writer(
        &self,
        context: &mut FloconContext,
        w: &mut dyn Write,
    ) -> io::Result<usize> {
        self.source.read_to(context, w)
    }

    /// Convenience wrapper around concrete_data_to_writer() which will give
    /// you the vector straight away instead of expecting you to create it.
    pub fn concrete_data(&self, context: &mut FloconContext) -> Vec<u8> {
        let mut v = Vec::new();
        self.concrete_data_to_writer(context, &mut v).unwrap();
        v
    }

    /// Generates a new block clipped within the given boundaries
    pub fn clip(&self, first_byte: u64, last_byte: u64) -> Result<WorkingBlock, String> {
        assert!(first_byte <= last_byte, "first_byte must be <= last_byte");

        let clipped_first = self.first_byte.max(first_byte);
        let clipped_last = self.last_byte.min(last_byte);

        if clipped_first > clipped_last {
            return Err("Clipping range does not intersect with block range".to_string());
        }

        let offset = clipped_first - self.first_byte;
        let size = clipped_last - clipped_first + 1;

        self.source
            .slice(offset, size)
            .map_err(|e| e.to_string())
            .map(|source| WorkingBlock::new(self.id, clipped_first, clipped_last, source))
    }

    /// Removes the given range from current block and returns the list of
    /// new blocks after this operation (either zero, one or two)
    pub fn remove(&self, first_byte: u64, last_byte: u64) -> Vec<WorkingBlock> {
        assert!(first_byte <= last_byte, "first_byte must be <= last_byte");

        if last_byte < self.first_byte || first_byte > self.last_byte {
            return vec![WorkingBlock::new(
                self.id,
                self.first_byte,
                self.last_byte,
                self.source.clone(),
            )];
        }

        if first_byte <= self.first_byte && last_byte >= self.last_byte {
            return vec![];
        }

        let mut result = Vec::new();

        if first_byte > self.first_byte {
            let left_last = (first_byte - 1).min(self.last_byte);
            let left_size = left_last - self.first_byte + 1;
            let left_source = self
                .source
                .slice(0, left_size)
                .map_err(|_| "Failed to slice source".to_string())
                .unwrap();

            result.push(WorkingBlock::new(
                None,
                self.first_byte,
                left_last,
                left_source,
            ));
        }

        if last_byte < self.last_byte {
            let right_first = (last_byte + 1).max(self.first_byte);
            let right_offset = right_first - self.first_byte;
            let right_size = self.last_byte - right_first + 1;
            let right_source = self
                .source
                .slice(right_offset, right_size)
                .map_err(|_| "Failed to slice source".to_string())
                .unwrap();

            result.push(WorkingBlock::new(
                None,
                right_first,
                self.last_byte,
                right_source,
            ));
        }

        result
    }
}

pub struct Sequence {
    pub blocks: Vec<WorkingBlock>,
}

impl Sequence {
    pub fn new(blocks: Vec<WorkingBlock>) -> Self {
        Self { blocks }
    }

    /// Writes the concrete data for the entire sequence to a writer, filling
    /// gaps with zeros. Returns the number of bytes written.
    pub fn concrete_data_to_writer(
        &self,
        context: &mut FloconContext,
        w: &mut dyn Write,
    ) -> io::Result<usize> {
        if self.blocks.is_empty() {
            return Ok(0);
        }

        let first_byte = self.blocks.first().unwrap().first_byte;

        let mut total_written = 0;
        let mut current_pos = first_byte;

        for block in &self.blocks {
            let gap_size = block.first_byte as i64 - current_pos as i64;

            if gap_size > 0 {
                total_written += Self::write_zeros(w, gap_size as usize)?;
            }

            total_written += block.concrete_data_to_writer(context, w)?;
            current_pos = block.last_byte + 1;
        }

        Ok(total_written)
    }

    /// Helper to write a given number of zero bytes to a writer.
    fn write_zeros(w: &mut dyn Write, mut count: usize) -> io::Result<usize> {
        const ZEROS: [u8; 4096] = [0; 4096];
        let original_count = count;

        while count > 0 {
            let n = count.min(ZEROS.len());
            w.write_all(&ZEROS[..n])?;
            count -= n;
        }

        Ok(original_count)
    }

    /// Returns the concrete data for the entire sequence, filling gaps with
    /// zeros
    pub fn concrete_data(&self, context: &mut FloconContext) -> Vec<u8> {
        let mut v = Vec::new();
        self.concrete_data_to_writer(context, &mut v).unwrap();
        v
    }

    /// Clips the sequence within the given boundaries
    pub fn clip(&self, first_byte: u64, last_byte: u64) -> Sequence {
        let mut clipped = Vec::new();

        for block in &self.blocks {
            if let Ok(clipped_block) = block.clip(first_byte, last_byte) {
                clipped.push(clipped_block);
            }
        }

        Sequence::new(clipped)
    }

    /// Inserts this block into the sequence and remove overlapping parts
    pub fn replace(&self, block: WorkingBlock) -> Sequence {
        let fb = block.first_byte;
        let lb = block.last_byte;
        let mut new_blocks = vec![block];

        for b in &self.blocks {
            new_blocks.extend(b.remove(fb, lb));
        }

        new_blocks.sort_by_key(|b| b.first_byte);
        Sequence::new(new_blocks)
    }
}
