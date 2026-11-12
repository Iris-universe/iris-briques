//! Capture d'écran de la fenêtre `main` (PrintWindow, y compris le contenu WebView2) -> PNG temporaire.
use tauri::{AppHandle, Manager};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

pub fn capture(app: &AppHandle) -> Result<String, String> {
    let w = app.get_webview_window("main").ok_or("fenêtre fermée")?;
    let hwnd = w.hwnd().map_err(|e| e.to_string())?.0 as HWND;
    let (wd, ht, mut px) = unsafe {
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rc);
        let (wd, ht) = (rc.right - rc.left, rc.bottom - rc.top);
        if wd <= 0 || ht <= 0 {
            return Err("fenêtre réduite".into());
        }
        let screen = GetDC(hwnd);
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, wd, ht);
        let old = SelectObject(mem, bmp);
        // PW_CLIENTONLY | PW_RENDERFULLCONTENT
        PrintWindow(hwnd, mem, 3);
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = wd;
        bi.bmiHeader.biHeight = -ht;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut px = vec![0u8; (wd * ht * 4) as usize];
        GetDIBits(mem, bmp, 0, ht as u32, px.as_mut_ptr() as *mut _, &mut bi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(hwnd, screen);
        (wd as u32, ht as u32, px)
    };
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
        p[3] = 255;
    }
    let id = app.try_state::<crate::Link>().map(|l| l.info.id).unwrap_or("iris");
    let f = std::env::temp_dir().join(format!("{id}-{}.png", chrono::Local::now().timestamp_millis()));
    let file = std::fs::File::create(&f).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), wd, ht);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut w| w.write_image_data(&px)).map_err(|e| e.to_string())?;
    Ok(f.to_string_lossy().to_string())
}
