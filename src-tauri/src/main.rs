fn main() {
    // WebKitGTK's DMA-BUF renderer shows a blank window on many NVIDIA setups.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
        && std::path::Path::new("/sys/module/nvidia").exists()
    {
        // SAFETY: still single-threaded; no other thread can read the environment yet.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    push_lib::run()
}
