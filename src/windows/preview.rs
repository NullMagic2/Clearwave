//! Render the real native controls into preview files without capturing a desktop.
//! This opt-in integration fixture uses temporary preferences and never starts audio.
//! Its GDI resources and windows remain owned by the test's UI thread.
use super::*;
use ::windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, GetDIBits, GetWindowDC, HBITMAP, HDC, ReleaseDC, SelectObject,
};
use ::windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use std::{io::Write, path::Path};

const PIXEL_BYTES: usize = 4;
const BITMAP_HEADER_BYTES: u32 = 54;
const BITMAP_INFO_BYTES: u32 = 40;
const BITMAP_BITS: u16 = 32;

#[derive(Default)]
struct Surface {
    window: HWND,
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: this owner releases only resources created by the preview test.
        // Restore the borrowed stock bitmap before deleting the owned bitmap/DC.
        // The source window stays alive until after this surface has been dropped.
        unsafe {
            if !self.previous.0.is_null() {
                SelectObject(self.memory, self.previous);
            }
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.memory);
            ReleaseDC(Some(self.window), self.screen);
        }
    }
}
fn render(window: HWND, path: &Path) -> Result<()> {
    let mut surface = Surface {
        window,
        ..Default::default()
    };
    let mut bounds = RECT::default();

    // SAFETY: this test owns the window, compatible DC and bitmap on one thread.
    // PrintWindow renders only that window hierarchy into the supplied memory DC.
    // GetDIBits writes within the explicitly sized, initialized pixel buffer.
    let (width, height, pixels) = unsafe {
        GetWindowRect(window, &mut bounds).map_err(os_error)?;
        let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
        let bytes = usize::try_from(width)
            .ok()
            .and_then(|w| {
                usize::try_from(height)
                    .ok()
                    .and_then(|h| w.checked_mul(h)?.checked_mul(PIXEL_BYTES))
            })
            .ok_or_else(|| Error::new("Invalid preview dimensions"))?;
        let mut pixels = vec![0u8; bytes];

        surface.screen = GetWindowDC(Some(window));
        surface.memory = CreateCompatibleDC(Some(surface.screen));
        surface.bitmap = CreateCompatibleBitmap(surface.screen, width, height);
        if surface.screen.0.is_null() || surface.memory.0.is_null() || surface.bitmap.0.is_null() {
            return Err(Error::new("Cannot allocate the native preview surface"));
        }
        surface.previous = SelectObject(surface.memory, HGDIOBJ(surface.bitmap.0));
        PrintWindow(window, surface.memory, PRINT_WINDOW_FLAGS::default())
            .ok()
            .map_err(os_error)?;
        SelectObject(surface.memory, surface.previous);
        surface.previous = HGDIOBJ::default();
        let mut information = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: BITMAP_INFO_BYTES,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: BITMAP_BITS,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        if GetDIBits(
            surface.memory,
            surface.bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut information,
            DIB_RGB_COLORS,
        ) != height
        {
            return Err(Error::new("Cannot read the native preview pixels"));
        }
        (width, height, pixels)
    };

    let payload = u32::try_from(pixels.len()).map_err(|_| Error::new("Preview is too large"))?;
    let mut file = std::fs::File::create(path)?;
    file.write_all(b"BM")?;
    for value in [
        BITMAP_HEADER_BYTES + payload,
        0,
        BITMAP_HEADER_BYTES,
        BITMAP_INFO_BYTES,
    ] {
        file.write_all(&value.to_le_bytes())?;
    }
    file.write_all(&width.to_le_bytes())?;
    file.write_all(&(-height).to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&BITMAP_BITS.to_le_bytes())?;
    for value in [BI_RGB.0, payload, 0, 0, 0, 0] {
        file.write_all(&value.to_le_bytes())?;
    }
    file.write_all(&pixels)?;
    Ok(())
}

#[test]
#[ignore = "requires an explicit preview output directory and a Windows display"]
fn render_native_windows_interface() {
    let output =
        PathBuf::from(std::env::var_os("CLEARWAVE_PREVIEW_DIRECTORY").expect("preview directory"));
    std::fs::create_dir_all(&output).unwrap();
    let _apartment = Apartment::new().unwrap();
    register_window_class().unwrap();
    let windows = OwnedWindows::create().unwrap();
    let (main, settings) = (windows.main, windows.settings);
    let preferences = output.join("preview-preferences.json");
    let app = App::create(windows, preferences, None).unwrap();
    app.layout(main);
    app.layout(settings);

    // SAFETY: these top-level HWNDs and their controls belong to this test thread.
    // Showing them initializes native theme painting; no input is synthesized.
    // The App owner destroys both windows after their preview files are written.
    unsafe {
        SetWindowPos(
            settings,
            None,
            0,
            0,
            SETTINGS_WIDTH,
            SETTINGS_HEIGHT,
            SWP_NOMOVE | SWP_NOZORDER,
        )
        .unwrap();
        let _ = ShowWindow(main, SW_SHOW);
        let _ = ShowWindow(settings, SW_SHOW);
    }
    app.layout(main);
    app.layout(settings);
    render(main, &output.join("windows-main.bmp")).unwrap();
    render(settings, &output.join("windows-settings.bmp")).unwrap();
    drop(app);
}
