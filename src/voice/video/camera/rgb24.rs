use std::borrow::Cow;

pub(super) fn packed_rows(
    bytes: &[u8],
    width: u32,
    height: u32,
    stride: i32,
) -> Option<Cow<'_, [u8]>> {
    let row_len = usize::try_from(width).ok()?.checked_mul(3)?;
    let height = usize::try_from(height).ok()?;
    let pitch = stride.unsigned_abs() as usize;
    if row_len == 0 || height == 0 || pitch < row_len {
        return None;
    }
    let last_row = (height - 1).checked_mul(pitch)?;
    let bytes = bytes.get(..last_row.checked_add(row_len)?)?;
    if stride > 0 && pitch == row_len {
        return Some(Cow::Borrowed(bytes));
    }

    let mut packed = Vec::with_capacity(row_len * height);
    for row in 0..height {
        let offset = if stride < 0 {
            last_row - row * pitch
        } else {
            row * pitch
        };
        packed.extend_from_slice(&bytes[offset..offset + row_len]);
    }
    Some(Cow::Owned(packed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_stride_preserves_pixel_order_and_removes_padding() {
        let top = [1, 2, 3, 4, 5, 6];
        let bottom = [7, 8, 9, 10, 11, 12];
        let upright = [top, bottom].concat();
        let top_down = [top.as_slice(), &[0, 0], &bottom].concat();
        let bottom_up = [bottom.as_slice(), &[0, 0], &top].concat();
        assert_eq!(packed_rows(&bottom_up, 2, 2, -8).unwrap().as_ref(), upright);
        assert_eq!(packed_rows(&top_down, 2, 2, 8).unwrap().as_ref(), upright);
        assert_eq!(packed_rows(&upright, 2, 2, 6).unwrap().as_ref(), upright);
        assert!(packed_rows(&bottom_up[..13], 2, 2, -8).is_none());
    }
}
