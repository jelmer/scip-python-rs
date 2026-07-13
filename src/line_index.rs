use ruff_text_size::{TextRange, TextSize};

/// Maps byte offsets in a source file to zero-based (line, column) pairs.
/// Columns are UTF-8 byte offsets from the start of the line, matching
/// SCIP's UTF8CodeUnitOffsetFromLineStart position encoding.
pub struct LineIndex {
    line_starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        LineIndex { line_starts }
    }

    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next_line) => next_line - 1,
        };
        (line as u32, offset - self.line_starts[line])
    }

    /// The byte offset of a zero-based (line, column) pair, the inverse of
    /// [`LineIndex::line_col`].
    pub fn offset(&self, line: i32, col: i32) -> TextSize {
        let start = self
            .line_starts
            .get(line as usize)
            .copied()
            .unwrap_or_default();
        TextSize::from(start + col as u32)
    }

    /// A SCIP occurrence range: three elements when the range is on a
    /// single line, four otherwise.
    pub fn range_vec(&self, range: TextRange) -> Vec<i32> {
        let (sl, sc) = self.line_col(range.start().into());
        let (el, ec) = self.line_col(range.end().into());
        if sl == el {
            vec![sl as i32, sc as i32, ec as i32]
        } else {
            vec![sl as i32, sc as i32, el as i32, ec as i32]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col() {
        let index = LineIndex::new("ab\ncd\n\nef");
        assert_eq!(index.line_col(0), (0, 0));
        assert_eq!(index.line_col(1), (0, 1));
        assert_eq!(index.line_col(3), (1, 0));
        assert_eq!(index.line_col(6), (2, 0));
        assert_eq!(index.line_col(7), (3, 0));
        assert_eq!(index.line_col(8), (3, 1));
    }
}
