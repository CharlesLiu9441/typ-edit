//! Shell-provided file-type icons for tree rows, cached per file extension.

#[cfg(windows)]
use std::collections::HashMap;
use std::path::Path;
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};

/// Returns a small file icon for `path`, extracted from the system shell and cached by
/// extension. `None` when extraction is unavailable.
#[cfg(windows)]
pub fn file_icon(ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
    let ext = extension_key(path);
    let cached = cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&ext)
        .cloned();
    if let Some(cached) = cached {
        return cached;
    }
    let tex = win::load_texture(ctx, path, &ext);
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ext, tex.clone());
    tex
}

/// Returns a small file icon for `path`, extracted from the system shell and cached by
/// extension. `None` when extraction is unavailable.
#[cfg(not(windows))]
pub fn file_icon(ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
    let _ = (ctx, path);
    None
}

/// Process-wide texture cache keyed by the lowercased extension (`""` for no extension).
/// Failures are cached as `None` so extraction is not retried every frame.
#[cfg(windows)]
fn cache() -> &'static Mutex<HashMap<String, Option<egui::TextureHandle>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<egui::TextureHandle>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cache key: the lowercased extension, or `""` when the path has none.
#[cfg(windows)]
fn extension_key(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

#[cfg(windows)]
mod win {
    use std::borrow::Cow;
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr;

    // Handle types are all opaque pointers.
    type Hdc = *mut c_void;
    type Hicon = *mut c_void;
    type Hbitmap = *mut c_void;
    type Hgdiobj = *mut c_void;

    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    const SHGFI_ICON: u32 = 0x0000_0100;
    const SHGFI_SMALLICON: u32 = 0x0000_0001;
    const SHGFI_USEFILEATTRIBUTES: u32 = 0x0000_0010;
    const DIB_RGB_COLORS: u32 = 0;
    const BI_RGB: u32 = 0;
    const DI_NORMAL: u32 = 3;

    const ICON_SIDE: usize = 16;
    const ICON_BYTES: usize = ICON_SIDE * ICON_SIDE * 4;

    #[repr(C)]
    struct ShFileInfoW {
        h_icon: Hicon,
        i_icon: i32,
        attributes: u32,
        display_name: [u16; 260],
        type_name: [u16; 80],
    }

    #[repr(C)]
    struct BitmapInfoHeader {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        bi_x_pels_per_meter: i32,
        bi_y_pels_per_meter: i32,
        bi_clr_used: u32,
        bi_clr_important: u32,
    }

    #[repr(C)]
    struct RgbQuad {
        rgb_blue: u8,
        rgb_green: u8,
        rgb_red: u8,
        rgb_reserved: u8,
    }

    #[repr(C)]
    struct BitmapInfo {
        bmi_header: BitmapInfoHeader,
        bmi_colors: [RgbQuad; 1],
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetFileInfoW(
            psz_path: *const u16,
            dw_file_attributes: u32,
            psfi: *mut ShFileInfoW,
            cb_file_info: u32,
            u_flags: u32,
        ) -> usize;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn DestroyIcon(hicon: Hicon) -> i32;
        fn DrawIconEx(
            hdc: Hdc,
            x_left: i32,
            y_top: i32,
            h_icon: Hicon,
            cx_width: i32,
            cy_height: i32,
            istep_if_ani_cur: u32,
            hbr_flicker_free_draw: *mut c_void,
            di_flags: u32,
        ) -> i32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(hdc: Hdc) -> Hdc;
        fn CreateDIBSection(
            hdc: Hdc,
            pbmi: *const BitmapInfo,
            usage: u32,
            ppv_bits: *mut *mut c_void,
            h_section: *mut c_void,
            offset: u32,
        ) -> Hbitmap;
        fn SelectObject(hdc: Hdc, h: Hgdiobj) -> Hgdiobj;
        fn DeleteObject(ho: Hgdiobj) -> i32;
        fn DeleteDC(hdc: Hdc) -> i32;
    }

    /// Owns the Win32 handles acquired during rasterization and releases them exactly once,
    /// in the order GDI requires: restore the previous bitmap, delete the DIB, delete the DC,
    /// then destroy the icon.
    struct Handles {
        hicon: Hicon,
        hdc: Hdc,
        hbitmap: Hbitmap,
        old_bitmap: Hgdiobj,
    }

    impl Drop for Handles {
        fn drop(&mut self) {
            // SAFETY: each handle is either null (never created) or a live handle produced by
            // the matching Win32 call, and this guard is the sole owner that releases it.
            unsafe {
                if !self.hdc.is_null() {
                    if !self.old_bitmap.is_null() && !is_gdi_error(self.old_bitmap) {
                        SelectObject(self.hdc, self.old_bitmap);
                    }
                    if !self.hbitmap.is_null() {
                        DeleteObject(self.hbitmap);
                    }
                    DeleteDC(self.hdc);
                }
                if !self.hicon.is_null() {
                    DestroyIcon(self.hicon);
                }
            }
        }
    }

    /// `SelectObject` reports failure with either `NULL` or `HGDI_ERROR` (`(HANDLE)-1`).
    fn is_gdi_error(h: Hgdiobj) -> bool {
        h as isize == -1
    }

    /// The shell's icon lookup and the GDI rasterization share process-global state and are not
    /// safe to run concurrently, so extractions are serialized. The UI thread is the only
    /// production caller, making this uncontended there.
    fn shell_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Builds the probe path: the real path when it exists, otherwise a synthetic name whose
    /// extension still drives the lookup (`SHGFI_USEFILEATTRIBUTES` never touches the disk).
    fn probe_path<'a>(path: &'a Path, ext: &str) -> Cow<'a, Path> {
        if path.exists() {
            Cow::Borrowed(path)
        } else if ext.is_empty() {
            Cow::Borrowed(Path::new("x"))
        } else {
            Cow::Owned(PathBuf::from(format!("x.{ext}")))
        }
    }

    /// Extracts the 16x16 shell icon for `path` and rasterizes it into an RGBA `ColorImage`.
    pub fn load_texture(
        ctx: &egui::Context,
        path: &Path,
        ext: &str,
    ) -> Option<egui::TextureHandle> {
        let image = extract_icon(&probe_path(path, ext))?;
        Some(ctx.load_texture(
            format!("sysicon.{ext}"),
            image,
            egui::TextureOptions::LINEAR,
        ))
    }

    /// Asks the shell for the file-type icon and converts the HICON into an RGBA image.
    /// Returns `None` on any Win32 failure; every handle is released via `Handles`.
    pub(crate) fn extract_icon(probe: &Path) -> Option<egui::ColorImage> {
        let _serial = shell_lock();
        let wide: Vec<u16> = probe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `wide` is a valid NUL-terminated UTF-16 buffer and `shfi` is a zeroed,
        // correctly sized SHFILEINFOW.
        let mut shfi: ShFileInfoW = unsafe { std::mem::zeroed() };
        let flags = SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES;
        let ok = unsafe {
            SHGetFileInfoW(
                wide.as_ptr(),
                FILE_ATTRIBUTE_NORMAL,
                &mut shfi,
                size_of::<ShFileInfoW>() as u32,
                flags,
            )
        };
        if ok == 0 || shfi.h_icon.is_null() {
            return None;
        }

        // From here the guard owns the icon and every GDI handle, releasing them on any exit.
        let mut handles = Handles {
            hicon: shfi.h_icon,
            hdc: ptr::null_mut(),
            hbitmap: ptr::null_mut(),
            old_bitmap: ptr::null_mut(),
        };

        // SAFETY: the sequential Win32 call contract is honored — each handle is created by the
        // preceding call and checked for failure before it is used or passed on.
        unsafe {
            handles.hdc = CreateCompatibleDC(ptr::null_mut());
            if handles.hdc.is_null() {
                return None;
            }

            // 32bpp top-down DIB so the byte order is BGRA and row 0 is the icon's top.
            let bmi = BitmapInfo {
                bmi_header: BitmapInfoHeader {
                    bi_size: size_of::<BitmapInfoHeader>() as u32,
                    bi_width: ICON_SIDE as i32,
                    bi_height: -(ICON_SIDE as i32),
                    bi_planes: 1,
                    bi_bit_count: 32,
                    bi_compression: BI_RGB,
                    bi_size_image: ICON_BYTES as u32,
                    bi_x_pels_per_meter: 0,
                    bi_y_pels_per_meter: 0,
                    bi_clr_used: 0,
                    bi_clr_important: 0,
                },
                bmi_colors: [RgbQuad {
                    rgb_blue: 0,
                    rgb_green: 0,
                    rgb_red: 0,
                    rgb_reserved: 0,
                }],
            };
            let mut bits: *mut c_void = ptr::null_mut();
            handles.hbitmap = CreateDIBSection(
                handles.hdc,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                ptr::null_mut(),
                0,
            );
            if handles.hbitmap.is_null() || bits.is_null() {
                return None;
            }

            handles.old_bitmap = SelectObject(handles.hdc, handles.hbitmap);
            if handles.old_bitmap.is_null() || is_gdi_error(handles.old_bitmap) {
                return None;
            }

            if DrawIconEx(
                handles.hdc,
                0,
                0,
                handles.hicon,
                ICON_SIDE as i32,
                ICON_SIDE as i32,
                0,
                ptr::null_mut(),
                DI_NORMAL,
            ) == 0
            {
                return None;
            }

            // Copy while the DIB section is still alive (the guard frees it on return).
            let src = std::slice::from_raw_parts(bits as *const u8, ICON_BYTES);
            let mut rgba = [0u8; ICON_BYTES];
            for (dst, px) in rgba
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<4>().0.iter())
            {
                let (b, g, r, mut a) = (px[0], px[1], px[2], px[3]);
                // Legacy mask-based icons carry no alpha; any drawn pixel is opaque.
                if a == 0 && (r | g | b) != 0 {
                    a = 255;
                }
                dst[0] = r;
                dst[1] = g;
                dst[2] = b;
                dst[3] = a;
            }

            Some(egui::ColorImage::from_rgba_unmultiplied(
                [ICON_SIDE, ICON_SIDE],
                &rgba,
            ))
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn extract_icon_yields_opaque_pixels_for_typ_and_pdf() {
        for ext in ["typ", "pdf"] {
            let probe = PathBuf::from(format!("x.{ext}"));
            let image = win::extract_icon(&probe).unwrap_or_else(|| panic!("no icon for .{ext}"));
            assert_eq!(image.size, [16, 16], "unexpected size for .{ext}");
            assert!(
                image.pixels.iter().any(|p| p.a() > 0),
                "icon for .{ext} is fully transparent"
            );
        }
    }

    #[test]
    fn file_icon_hits_cache_for_same_extension() {
        let ctx = egui::Context::default();
        let first = file_icon(&ctx, Path::new("cache_probe.typ")).expect("first icon");
        let second = file_icon(&ctx, Path::new("another_probe.typ")).expect("cached icon");
        assert_eq!(first.id(), second.id(), "cache did not reuse the texture");
    }

    #[test]
    fn extension_key_lowercases_and_defaults_to_empty() {
        assert_eq!(extension_key(Path::new("A.TYP")), "typ");
        assert_eq!(extension_key(Path::new("Makefile")), "");
    }

    #[test]
    fn repeated_extraction_does_not_leak_gdi_or_user_handles() {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetGuiResources(hprocess: *mut std::ffi::c_void, ui_flags: u32) -> u32;
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
        }
        const GR_GDIOBJECTS: u32 = 0;
        const GR_USEROBJECTS: u32 = 1;
        const ITERATIONS: usize = 50;
        const ALLOWED_DRIFT: u32 = 10;

        for _ in 0..3 {
            let _ = win::extract_icon(Path::new("x.typ"));
        }
        let gdi_before = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        let user_before = unsafe { GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS) };
        for _ in 0..ITERATIONS {
            let _ = win::extract_icon(Path::new("x.typ"));
        }
        let gdi_after = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        let user_after = unsafe { GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS) };
        assert!(
            gdi_after <= gdi_before + ALLOWED_DRIFT,
            "GDI handles grew across {ITERATIONS} extractions: {gdi_before} -> {gdi_after}"
        );
        assert!(
            user_after <= user_before + ALLOWED_DRIFT,
            "USER handles grew across {ITERATIONS} extractions: {user_before} -> {user_after}"
        );
    }
}
