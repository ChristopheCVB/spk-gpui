use image::{Frame, RgbaImage};

pub fn bgra_frame(mut image: RgbaImage) -> Frame {
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Frame::new(image)
}
