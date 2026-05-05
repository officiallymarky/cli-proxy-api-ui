use image::ImageReader;

/// Decode the embedded tray icon PNG into raw RGBA bytes.
pub fn decode_tray_icon() -> (u32, u32, Vec<u8>) {
    let bytes = include_bytes!("../icons/icon.png");
    let reader = ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
        .with_guessed_format()
        .expect("icon format");
    let img = reader.decode().expect("icon decode");
    let rgba = img.to_rgba8();
    let w = rgba.width();
    let h = rgba.height();
    (w, h, rgba.into_raw())
}


