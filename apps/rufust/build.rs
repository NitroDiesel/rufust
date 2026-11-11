fn main() {
    slint_build::compile("ui/appwindow.slint").expect("Slint build failed");
    #[cfg(windows)]
    windows_resources();
}

/// Give `rufust.exe` the original Rufus icon and an `asInvoker` manifest: the
/// window runs unelevated and asks UAC only for its disk worker.
#[cfg(windows)]
fn windows_resources() {
    use std::path::{Path, PathBuf};

    let icons = Path::new("../../assets/icons");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    // An ICO whose entries are the original PNGs (supported since Vista).
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let images = sizes
        .iter()
        .map(|size| {
            let path = icons.join(format!("rufust-{size}.png"));
            println!("cargo:rerun-if-changed={}", path.display());
            std::fs::read(&path).expect("icon PNG")
        })
        .collect::<Vec<_>>();
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (size, image) in sizes.iter().zip(&images) {
        let side = if *size >= 256 { 0 } else { *size as u8 };
        ico.extend_from_slice(&[side, side, 0, 0, 1, 0, 32, 0]);
        ico.extend_from_slice(&(image.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in &images {
        ico.extend_from_slice(image);
    }
    let icon = out.join("rufust.ico");
    std::fs::write(&icon, ico).expect("write icon");

    let manifest = out.join("rufust.exe.manifest");
    std::fs::write(&manifest, MANIFEST).expect("write manifest");
    let quote = |path: &Path| path.display().to_string().replace('\\', "/");
    let script = out.join("rufust.rc");
    std::fs::write(
        &script,
        format!(
            "1 ICON \"{}\"\n1 24 \"{}\"\n",
            quote(&icon),
            quote(&manifest)
        ),
    )
    .expect("write resource script");
    embed_resource::compile(&script, embed_resource::NONE)
        .manifest_required()
        .expect("compile Windows resources");
}

#[cfg(windows)]
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="NitroDiesel.Rufust" version="1.0.0.0"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;
