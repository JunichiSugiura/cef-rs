//! Centralized CEF view texture color/alpha policy.

/// CEF view buffers represent sRGB web content in BGRA bytes.
///
/// We keep the underlying texture as UNORM and sample through an sRGB view.
/// This avoids touching upstream `cef/` while still getting correct colors.
pub const CEF_VIEW_TEXTURE_BASE_FORMAT_BGRA: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
pub const CEF_VIEW_TEXTURE_VIEW_FORMAT_BGRA: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;
pub const CEF_VIEW_TEXTURE_FORMATS_BGRA: [wgpu::TextureFormat; 1] =
    [CEF_VIEW_TEXTURE_VIEW_FORMAT_BGRA];

/// vmux renders embedded CEF content into an opaque window.
pub const OUTPUT_IS_OPAQUE: bool = true;

