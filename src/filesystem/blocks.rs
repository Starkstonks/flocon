use crate::models::block::{Block, NewBlock};
use fuse_backend_rs::api::filesystem::ZeroCopyWriter;
use std::io;
use std::io::Write;

#[derive(Debug, Clone, PartialEq)]
pub struct WorkingBlock {
    pub id: Option<i32>,
    pub first_byte: i32,
    pub last_byte: i32,
    pub data: Option<Vec<u8>>,
}

impl WorkingBlock {
    pub fn new(id: Option<i32>, first_byte: i32, last_byte: i32, data: Option<Vec<u8>>) -> Self {
        let block = Self {
            id,
            first_byte,
            last_byte,
            data,
        };

        #[cfg(debug_assertions)]
        {
            if let Some(ref data) = block.data {
                debug_assert_eq!(
                    data.len() as i32,
                    block.last_byte - block.first_byte + 1,
                    "Data length is inconsistent with block coordinates"
                );
            }
        }

        block
    }

    /// Create WorkingBlock from a Diesel Block model
    pub fn from_model(block: Block) -> Self {
        Self {
            id: Some(block.id),
            first_byte: block.first_byte,
            last_byte: block.last_byte,
            data: block.data,
        }
    }

    /// Convert to NewBlock for insertion (requires inode_id)
    pub fn to_new_block(&self, inode_id: i32) -> NewBlock {
        NewBlock {
            inode_id,
            first_byte: self.first_byte,
            last_byte: self.last_byte,
            data: self.data.as_deref(),
        }
    }

    /// The data might be actual data but it might also be zero-filled. When
    /// it is used to "export" the data to other systems that don't have that
    /// convention, we need to materialize those zeros.
    pub fn concrete_data(&self) -> Vec<u8> {
        match &self.data {
            Some(data) => data.clone(),
            None => vec![0u8; (self.last_byte - self.first_byte + 1) as usize],
        }
    }

    /// Generates a new block clipped within the given boundaries
    pub fn clip(&self, first_byte: i32, last_byte: i32) -> Result<WorkingBlock, String> {
        assert!(first_byte <= last_byte, "first_byte must be <= last_byte");

        let clipped_first = self.first_byte.max(first_byte);
        let clipped_last = self.last_byte.min(last_byte);

        if clipped_first > clipped_last {
            return Err("Clipping range does not intersect with block range".to_string());
        }

        if clipped_first == self.first_byte && clipped_last == self.last_byte {
            return Ok(self.clone());
        }

        let clipped_data = self.data.as_ref().map(|data| {
            let start_offset = (clipped_first - self.first_byte) as usize;
            let end_offset = (clipped_last - self.first_byte + 1) as usize;
            data[start_offset..end_offset].to_vec()
        });

        Ok(WorkingBlock::new(
            None,
            clipped_first,
            clipped_last,
            clipped_data,
        ))
    }

    /// Removes the given range from current block and returns the list of
    /// new blocks after this operation (either zero, one or two)
    pub fn remove(&self, first_byte: i32, last_byte: i32) -> Vec<WorkingBlock> {
        assert!(first_byte <= last_byte, "first_byte must be <= last_byte");

        if last_byte < self.first_byte || first_byte > self.last_byte {
            return vec![self.clone()];
        }

        if first_byte <= self.first_byte && last_byte >= self.last_byte {
            return vec![];
        }

        let mut result = Vec::new();

        if first_byte > self.first_byte {
            let left_last = (first_byte - 1).min(self.last_byte);
            let left_data = self.data.as_ref().map(|data| {
                let end = (left_last - self.first_byte + 1) as usize;
                data[..end].to_vec()
            });

            result.push(WorkingBlock::new(
                None,
                self.first_byte,
                left_last,
                left_data,
            ));
        }

        if last_byte < self.last_byte {
            let right_first = (last_byte + 1).max(self.first_byte);
            let right_data = self.data.as_ref().map(|data| {
                let start_offset = (right_first - self.first_byte) as usize;
                data[start_offset..].to_vec()
            });

            result.push(WorkingBlock::new(
                None,
                right_first,
                self.last_byte,
                right_data,
            ));
        }

        result
    }
}

#[derive(Debug, Clone)]
pub struct Sequence {
    pub blocks: Vec<WorkingBlock>,
}

impl Sequence {
    pub fn new(blocks: Vec<WorkingBlock>) -> Self {
        Self { blocks }
    }

    /// Writes the concrete data for the entire sequence to a writer, filling
    /// gaps with zeros. Returns the number of bytes written.
    pub fn concrete_data_to_writer(&self, w: &mut dyn Write) -> io::Result<usize> {
        if self.blocks.is_empty() {
            return Ok(0);
        }

        let first_byte = self.blocks.first().unwrap().first_byte;

        let mut total_written = 0;
        let mut current_pos = first_byte;

        for block in &self.blocks {
            // Fill gap before this block with zeros
            let gap_size = block.first_byte as i64 - current_pos as i64;
            if gap_size > 0 {
                total_written += Self::write_zeros(w, gap_size as usize)?;
            }

            // Add block data
            match &block.data {
                Some(data) => {
                    w.write_all(data)?;
                    total_written += data.len();
                }
                None => {
                    let block_size = (block.last_byte - block.first_byte + 1) as usize;
                    total_written += Self::write_zeros(w, block_size)?;
                }
            }

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
    pub fn concrete_data(&self) -> Vec<u8> {
        let mut v = Vec::new();
        self.concrete_data_to_writer(&mut v).unwrap();
        v
    }

    /// Clips the sequence within the given boundaries
    pub fn clip(&self, first_byte: i32, last_byte: i32) -> Sequence {
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
        let mut new_blocks = vec![block.clone()];

        for b in &self.blocks {
            new_blocks.extend(b.remove(block.first_byte, block.last_byte));
        }

        new_blocks.sort_by_key(|b| b.first_byte);
        Sequence::new(new_blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_clip() {
        let fb = 10;
        let lb = 20;

        // Block before range
        let b = WorkingBlock::new(None, 0, 5, None);
        assert!(b.clip(fb, lb).is_err());
        assert_eq!(
            b.clip(fb, lb).unwrap_err(),
            "Clipping range does not intersect with block range"
        );

        // Block cut by range start
        let b = WorkingBlock::new(None, 5, 14, Some(b"0123456789".to_vec()));
        let bc = b.clip(fb, lb).unwrap();
        assert_eq!(bc.first_byte, 10);
        assert_eq!(bc.last_byte, 14);
        assert_eq!(bc.data.as_ref().unwrap(), b"56789");

        // Block cut by range end
        let b = WorkingBlock::new(None, 17, 26, Some(b"0123456789".to_vec()));
        let bc = b.clip(fb, lb).unwrap();
        assert_eq!(bc.first_byte, 17);
        assert_eq!(bc.last_byte, 20);
        assert_eq!(bc.data.as_ref().unwrap(), b"0123");

        // Block within range
        let b = WorkingBlock::new(None, 10, 19, Some(b"0123456789".to_vec()));
        let bc = b.clip(fb, lb).unwrap();
        assert_eq!(b, bc);

        // Range within block
        let b = WorkingBlock::new(None, 10, 19, Some(b"0123456789".to_vec()));
        let bc = b.clip(11, 18).unwrap();
        assert_eq!(bc.first_byte, 11);
        assert_eq!(bc.last_byte, 18);
        assert_eq!(bc.data.as_ref().unwrap(), b"12345678");
    }

    #[test]
    fn test_block_concrete_data() {
        // Block within range
        let b = WorkingBlock::new(None, 10, 19, None);
        let bc = b.clip(10, 20).unwrap();
        assert_eq!(bc.concrete_data(), vec![0u8; 10]);

        // Range within block
        let b = WorkingBlock::new(None, 10, 19, None);
        let bc = b.clip(11, 18).unwrap();
        assert_eq!(bc.concrete_data(), vec![0u8; 8]);

        // Range within block (with data)
        let b = WorkingBlock::new(None, 10, 19, Some(b"0123456789".to_vec()));
        let bc = b.clip(11, 18).unwrap();
        assert_eq!(bc.concrete_data(), b"12345678");
    }

    #[test]
    fn test_clip_sequence() {
        let b1 = WorkingBlock::new(Some(1), 0, 10, Some(b"abcdefghijk".to_vec()));
        let b2 = WorkingBlock::new(Some(2), 11, 16, Some(b"lmnopq".to_vec()));
        let b3 = WorkingBlock::new(Some(3), 17, 25, Some(b"rstuvwxyz".to_vec()));
        let s = Sequence::new(vec![b1, b2, b3]);

        let s_prime = s.clip(10, 20);
        assert_eq!(s_prime.blocks.len(), 3);

        assert_eq!(s_prime.blocks[0].id, None);
        assert_eq!(s_prime.blocks[0].first_byte, 10);
        assert_eq!(s_prime.blocks[0].last_byte, 10);
        assert_eq!(s_prime.blocks[0].concrete_data(), b"k");

        assert_eq!(s_prime.blocks[1].id, Some(2));
        assert_eq!(s_prime.blocks[1].first_byte, 11);
        assert_eq!(s_prime.blocks[1].last_byte, 16);
        assert_eq!(s_prime.blocks[1].concrete_data(), b"lmnopq");

        assert_eq!(s_prime.blocks[2].id, None);
        assert_eq!(s_prime.blocks[2].first_byte, 17);
        assert_eq!(s_prime.blocks[2].last_byte, 20);
        assert_eq!(s_prime.blocks[2].concrete_data(), b"rstu");
    }

    #[test]
    fn test_remove_block() {
        let b = WorkingBlock::new(Some(1), 5, 14, Some(b"0123456789".to_vec()));

        // No intersect
        let result = b.remove(0, 4);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], b);

        // Full intersect
        let result = b.remove(5, 14);
        assert!(result.is_empty());

        // Clip left
        let result = b.remove(3, 7);
        assert_eq!(result.len(), 1);
        let b1 = &result[0];
        assert_eq!(b1.id, None);
        assert_eq!(b1.first_byte, 8);
        assert_eq!(b1.last_byte, 14);
        assert_eq!(b1.concrete_data(), b"3456789");

        // Clip right
        let result = b.remove(12, 14);
        assert_eq!(result.len(), 1);
        let b1 = &result[0];
        assert_eq!(b1.id, None);
        assert_eq!(b1.first_byte, 5);
        assert_eq!(b1.last_byte, 11);
        assert_eq!(b1.concrete_data(), b"0123456");

        // Punch a hole
        let result = b.remove(7, 10);
        assert_eq!(result.len(), 2);
        let b1 = &result[0];
        let b2 = &result[1];
        assert_eq!(b1.id, None);
        assert_eq!(b1.first_byte, 5);
        assert_eq!(b1.last_byte, 6);
        assert_eq!(b1.concrete_data(), b"01");
        assert_eq!(b2.id, None);
        assert_eq!(b2.first_byte, 11);
        assert_eq!(b2.last_byte, 14);
        assert_eq!(b2.concrete_data(), b"6789");
    }

    #[test]
    fn test_sequence_concrete_data() {
        // Empty sequence
        let s = Sequence::new(vec![]);
        assert_eq!(s.concrete_data(), Vec::<u8>::new());

        // Single block
        let b1 = WorkingBlock::new(None, 5, 9, Some(b"hello".to_vec()));
        let s = Sequence::new(vec![b1]);
        assert_eq!(s.concrete_data(), b"hello");

        // Multiple blocks with gaps
        let b1 = WorkingBlock::new(None, 0, 4, Some(b"hello".to_vec()));
        let b2 = WorkingBlock::new(None, 10, 14, Some(b"world".to_vec()));
        let s = Sequence::new(vec![b1, b2]);
        assert_eq!(s.concrete_data(), b"hello\0\0\0\0\0world");

        // Blocks with None data (should be zeros)
        let b1 = WorkingBlock::new(None, 0, 2, None);
        let b2 = WorkingBlock::new(None, 3, 5, Some(b"abc".to_vec()));
        let s = Sequence::new(vec![b1, b2]);
        assert_eq!(s.concrete_data(), b"\0\0\0abc");
    }
}
