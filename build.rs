fn main() {
    println!("cargo:rerun-if-changed=icon.ico");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("icon.ico");
        resource.set("ProductName", "Vidown");
        resource.set("FileDescription", "Vidown");
        if let Err(err) = resource.compile() {
            println!("cargo:warning=failed to embed icon: {err}");
        }
    }
}
