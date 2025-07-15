use crate::filesystem::FloconContext;
use std::io;
use std::io::Read;
use std::io::{Seek, Write};
use std::sync::Arc;

pub trait DataSource {
    /// Reads the content from the data source into the provided writer
    fn read_to(&self, context: &mut FloconContext, w: &mut dyn Write) -> io::Result<usize>;

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
    fn read_to(&self, _context: &mut FloconContext, w: &mut dyn Write) -> io::Result<usize> {
        w.write_all(self.as_slice())?;
        Ok(self.size as usize)
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
    fn read_to(&self, _context: &mut FloconContext, w: &mut dyn Write) -> io::Result<usize> {
        let zeros = vec![0u8; self.size as usize];
        w.write_all(&zeros)?;
        Ok(self.size as usize)
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
        let new_id = if clipped_first == self.first_byte && clipped_last == self.last_byte {
            self.id
        } else {
            None
        };

        self.source
            .slice(offset, size)
            .map_err(|e| e.to_string())
            .map(|source| WorkingBlock::new(new_id, clipped_first, clipped_last, source))
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
    #[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use r2d2::Pool;
    use r2d2_sqlite::SqliteConnectionManager;
    use rusqlite::{OpenFlags, params};
    use std::sync::Arc;

    // Helper to create a test context with in-memory SQLite
    fn create_test_context() -> FloconContext {
        let manager = SqliteConnectionManager::memory()
            .with_flags(OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE);
        let pool = Pool::new(manager).unwrap();
        let conn = pool.get().unwrap();

        // Create test table
        conn.execute(
            "CREATE TABLE block (
                id INTEGER PRIMARY KEY,
                data BLOB
            )",
            [],
        )
        .unwrap();

        FloconContext { conn }
    }

    /// Inserts a test block with the provided data and returns the ID of said
    /// block
    fn insert_test_block(context: &mut FloconContext, data: &[u8]) -> u64 {
        context
            .conn
            .execute("INSERT INTO block (data) VALUES (?1)", params![data])
            .unwrap();

        context.conn.last_insert_rowid() as u64
    }

    #[test]
    fn test_memory_data_source_basic() {
        let data = Arc::new(vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let source = MemoryDataSource::new(data.clone(), 0, 10).unwrap();

        assert_eq!(source.size, 10);

        let mut context = create_test_context();
        let mut output = Vec::new();
        let bytes_written = source.read_to(&mut context, &mut output).unwrap();

        assert_eq!(bytes_written, 10);
        assert_eq!(output, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn test_memory_data_source_slice() {
        let data = Arc::new(vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let source = MemoryDataSource::new(data.clone(), 0, 10).unwrap();

        // Slice from offset 2, length 5
        let sliced = source.slice(2, 5).unwrap();

        let mut context = create_test_context();
        let mut output = Vec::new();
        sliced.read_to(&mut context, &mut output).unwrap();

        assert_eq!(output, vec![2, 3, 4, 5, 6]);
    }

    #[test]
    fn test_memory_data_source_invalid_bounds() {
        let data = Arc::new(vec![0, 1, 2, 3, 4]);

        // Offset out of bounds
        assert!(MemoryDataSource::new(data.clone(), 10, 1).is_err());

        // Size exceeds available data
        assert!(MemoryDataSource::new(data.clone(), 0, 10).is_err());

        // Valid source, invalid slice
        let source = MemoryDataSource::new(data.clone(), 0, 5).unwrap();
        assert!(source.slice(3, 3).is_err()); // Would exceed bounds
    }

    #[test]
    fn test_zero_data_source() {
        let source = ZeroDataSource::new(10);
        assert_eq!(source.size, 10);

        let mut context = create_test_context();
        let mut output = Vec::new();
        let bytes_written = source.read_to(&mut context, &mut output).unwrap();

        assert_eq!(bytes_written, 10);
        assert_eq!(output, vec![0; 10]);
    }

    #[test]
    fn test_zero_data_source_slice() {
        let source = ZeroDataSource::new(20);
        let sliced = source.slice(5, 10).unwrap();

        let mut context = create_test_context();
        let mut output = Vec::new();
        sliced.read_to(&mut context, &mut output).unwrap();

        assert_eq!(output, vec![0; 10]);
    }

    #[test]
    fn test_sqlite_data_source() {
        let mut context = create_test_context();

        // Insert test data
        let block_id = insert_test_block(&mut context, b"0123456789");

        let source = SqliteDataSource::new(block_id, 0, 10);
        assert_eq!(source.size, 10);

        let mut output = Vec::new();
        let bytes_written = source.read_to(&mut context, &mut output).unwrap();
        let _output_as_string = String::from_utf8(output.clone()).unwrap();

        assert_eq!(bytes_written, 10);
        assert_eq!(output, b"0123456789");
    }

    #[test]
    fn test_sqlite_data_source_slice() {
        let mut context = create_test_context();

        // Insert test data
        let block_id = insert_test_block(&mut context, b"0123456789");

        let source = SqliteDataSource::new(block_id, 0, 10);
        let sliced = source.slice(3, 4).unwrap();

        let mut output = Vec::new();
        sliced.read_to(&mut context, &mut output).unwrap();

        assert_eq!(output, b"3456");
    }

    #[test]
    fn test_working_block_clip() {
        let mut context = create_test_context();

        // Block before range
        let data = MemoryDataSource::from_data(vec![0, 1, 2, 3, 4, 5]).unwrap();
        let block = WorkingBlock::new(None, 0, 5, Box::new(data));
        let result = block.clip(10, 20);
        assert!(result.is_err());

        // Block cut by range start
        let data = MemoryDataSource::from_data(b"0123456789".to_vec()).unwrap();
        let block = WorkingBlock::new(None, 5, 14, Box::new(data));
        let clipped = block.clip(10, 20).unwrap();
        assert_eq!(clipped.first_byte, 10);
        assert_eq!(clipped.last_byte, 14);
        assert_eq!(clipped.concrete_data(&mut context), b"56789");

        // Block cut by range end
        let data = MemoryDataSource::from_data(b"0123456789".to_vec()).unwrap();
        let block = WorkingBlock::new(None, 17, 26, Box::new(data));
        let clipped = block.clip(10, 20).unwrap();
        assert_eq!(clipped.first_byte, 17);
        assert_eq!(clipped.last_byte, 20);
        assert_eq!(clipped.concrete_data(&mut context), b"0123");

        // Block within range
        let data = MemoryDataSource::from_data(b"0123456789".to_vec()).unwrap();
        let block = WorkingBlock::new(None, 10, 19, Box::new(data));
        let clipped = block.clip(10, 20).unwrap();
        assert_eq!(clipped.first_byte, 10);
        assert_eq!(clipped.last_byte, 19);
        assert_eq!(clipped.concrete_data(&mut context), b"0123456789");

        // Range within block
        let data = MemoryDataSource::from_data(b"0123456789".to_vec()).unwrap();
        let block = WorkingBlock::new(None, 10, 19, Box::new(data));
        let clipped = block.clip(11, 18).unwrap();
        assert_eq!(clipped.first_byte, 11);
        assert_eq!(clipped.last_byte, 18);
        assert_eq!(clipped.concrete_data(&mut context), b"12345678");
    }

    #[test]
    fn test_working_block_concrete_data() {
        let mut context = create_test_context();

        // Block with zeros
        let source = ZeroDataSource::new(10);
        let block = WorkingBlock::new(None, 10, 19, Box::new(source));
        let clipped = block.clip(10, 20).unwrap();
        assert_eq!(clipped.concrete_data(&mut context), vec![0; 10]);

        // Range within block
        let source = ZeroDataSource::new(10);
        let block = WorkingBlock::new(None, 10, 19, Box::new(source));
        let clipped = block.clip(11, 18).unwrap();
        assert_eq!(clipped.concrete_data(&mut context), vec![0; 8]);

        // Range within block (with data)
        let data = MemoryDataSource::from_data(b"0123456789".to_vec()).unwrap();
        let block = WorkingBlock::new(None, 10, 19, Box::new(data));
        let clipped = block.clip(11, 18).unwrap();
        assert_eq!(clipped.concrete_data(&mut context), b"12345678");
    }

    #[test]
    fn test_working_block_remove() {
        let mut context = create_test_context();

        let data = MemoryDataSource::from_data(b"0123456789".to_vec()).unwrap();
        let block = WorkingBlock::new(Some(1), 5, 14, Box::new(data));

        // No intersect
        let result = block.remove(0, 4);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].first_byte, 5);
        assert_eq!(result[0].last_byte, 14);

        // Full intersect
        let result = block.remove(5, 14);
        assert_eq!(result.len(), 0);

        // Clip left
        let result = block.remove(3, 7);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, None);
        assert_eq!(result[0].first_byte, 8);
        assert_eq!(result[0].last_byte, 14);
        assert_eq!(result[0].concrete_data(&mut context), b"3456789");

        // Clip right
        let result = block.remove(12, 14);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, None);
        assert_eq!(result[0].first_byte, 5);
        assert_eq!(result[0].last_byte, 11);
        assert_eq!(result[0].concrete_data(&mut context), b"0123456");

        // Punch a hole
        let result = block.remove(7, 10);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, None);
        assert_eq!(result[0].first_byte, 5);
        assert_eq!(result[0].last_byte, 6);
        assert_eq!(result[0].concrete_data(&mut context), b"01");
        assert_eq!(result[1].id, None);
        assert_eq!(result[1].first_byte, 11);
        assert_eq!(result[1].last_byte, 14);
        assert_eq!(result[1].concrete_data(&mut context), b"6789");
    }

    #[test]
    fn test_sequence_clip() {
        let mut context = create_test_context();

        let b1_data = MemoryDataSource::from_data(b"abcdefghijk".to_vec()).unwrap();
        let b1 = WorkingBlock::new(Some(1), 0, 10, Box::new(b1_data));

        let b2_data = MemoryDataSource::from_data(b"lmnopq".to_vec()).unwrap();
        let b2 = WorkingBlock::new(Some(2), 11, 16, Box::new(b2_data));

        let b3_data = MemoryDataSource::from_data(b"rstuvwxyz".to_vec()).unwrap();
        let b3 = WorkingBlock::new(Some(3), 17, 25, Box::new(b3_data));

        let sequence = Sequence::new(vec![b1, b2, b3]);

        let clipped = sequence.clip(10, 20);
        assert_eq!(clipped.blocks.len(), 3);

        assert_eq!(clipped.blocks[0].id, None);
        assert_eq!(clipped.blocks[0].first_byte, 10);
        assert_eq!(clipped.blocks[0].last_byte, 10);
        assert_eq!(clipped.blocks[0].concrete_data(&mut context), b"k");

        assert_eq!(clipped.blocks[1].id, Some(2));
        assert_eq!(clipped.blocks[1].first_byte, 11);
        assert_eq!(clipped.blocks[1].last_byte, 16);
        assert_eq!(clipped.blocks[1].concrete_data(&mut context), b"lmnopq");

        assert_eq!(clipped.blocks[2].id, None);
        assert_eq!(clipped.blocks[2].first_byte, 17);
        assert_eq!(clipped.blocks[2].last_byte, 20);
        assert_eq!(clipped.blocks[2].concrete_data(&mut context), b"rstu");
    }

    #[test]
    fn test_sequence_concrete_data_with_gaps() {
        let mut context = create_test_context();

        let b1_data = MemoryDataSource::from_data(b"abc".to_vec()).unwrap();
        let b1 = WorkingBlock::new(None, 0, 2, Box::new(b1_data));

        let b2_data = MemoryDataSource::from_data(b"def".to_vec()).unwrap();
        let b2 = WorkingBlock::new(None, 5, 7, Box::new(b2_data));

        let sequence = Sequence::new(vec![b1, b2]);
        let data = sequence.concrete_data(&mut context);

        // Should be: "abc" + 2 zeros + "def"
        assert_eq!(data, b"abc\0\0def");
    }

    #[test]
    fn test_sequence_replace() {
        let mut context = create_test_context();

        let b1_data = MemoryDataSource::from_data(b"abcdefghijk".to_vec()).unwrap();
        let b1 = WorkingBlock::new(Some(1), 0, 10, Box::new(b1_data));

        let b2_data = MemoryDataSource::from_data(b"lmnopq".to_vec()).unwrap();
        let b2 = WorkingBlock::new(Some(2), 11, 16, Box::new(b2_data));

        let sequence = Sequence::new(vec![b1, b2]);

        // Replace overlapping parts
        let new_data = MemoryDataSource::from_data(b"XXX".to_vec()).unwrap();
        let new_block = WorkingBlock::new(None, 8, 10, Box::new(new_data));

        let replaced = sequence.replace(new_block);
        assert_eq!(replaced.blocks.len(), 3);

        // First block should be truncated
        assert_eq!(replaced.blocks[0].first_byte, 0);
        assert_eq!(replaced.blocks[0].last_byte, 7);
        assert_eq!(replaced.blocks[0].concrete_data(&mut context), b"abcdefgh");

        // New block
        assert_eq!(replaced.blocks[1].first_byte, 8);
        assert_eq!(replaced.blocks[1].last_byte, 10);
        assert_eq!(replaced.blocks[1].concrete_data(&mut context), b"XXX");

        // Second original block unchanged
        assert_eq!(replaced.blocks[2].first_byte, 11);
        assert_eq!(replaced.blocks[2].last_byte, 16);
        assert_eq!(replaced.blocks[2].concrete_data(&mut context), b"lmnopq");
    }
}
