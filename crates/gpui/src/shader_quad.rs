use crate::{Bounds, ContentMask, DevicePixels, DrawOrder, Primitive, ScaledPixels, Size};
use anyhow::Result;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// The most textures one shader quad can read.
pub const MAX_SHADER_TEXTURES: usize = 4;

/// The largest parameter block one shader quad can receive, in bytes.
pub const MAX_SHADER_PARAMS: usize = 256;

/// A WGSL fragment program that [`crate::Window::paint_shader`] runs over a rectangle.
///
/// The source defines `fn shade(input: ShadeInput) -> vec4<f32>` returning straight alpha,
/// and optionally `struct ShaderParams`. It reads `params` and the textures `t0`..`t3`
/// with `textureLoad`. Each renderer compiles a program once per window.
#[derive(Clone, Debug)]
pub struct ShaderProgram {
    id: u64,
    source: Arc<str>,
}

impl ShaderProgram {
    /// Creates a program from WGSL source.
    pub fn new(source: impl Into<Arc<str>>) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            source: source.into(),
        }
    }

    /// Identifies this program for renderer-side caches.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The WGSL source given to [`Self::new`].
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// A texture owned by the application and created with [`crate::Window::create_texture`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextureId(pub u64);

/// Pixel layouts available to application textures.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    /// 8-bit BGRA, read as normalized floats without sRGB decoding.
    Bgra8Unorm,
    /// 16-bit RGBA, read as normalized floats.
    Rgba16Unorm,
}

impl TextureFormat {
    /// Bytes per pixel.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            TextureFormat::Bgra8Unorm => 4,
            TextureFormat::Rgba16Unorm => 8,
        }
    }
}

/// Renderer-side storage for application textures, one per window.
pub trait PlatformTextures {
    /// Creates a texture of the given size and format.
    fn create(&self, size: Size<DevicePixels>, format: TextureFormat) -> Result<TextureId>;

    /// Replaces `rect` with `bytes`, whose rows are `bytes_per_row` apart. The new contents
    /// are visible to the next frame drawn.
    fn update(
        &self,
        id: TextureId,
        rect: Bounds<DevicePixels>,
        bytes: &[u8],
        bytes_per_row: usize,
    ) -> Result<()>;

    /// Releases a texture. Frames already submitted keep it alive until they finish.
    fn remove(&self, id: TextureId);

    /// Changes whenever all textures were lost and must be created and uploaded again.
    fn generation(&self) -> u64;
}

#[derive(Clone, Debug)]
#[expect(missing_docs)]
pub struct PaintShader {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub opacity: f32,
    pub program: ShaderProgram,
    pub params: Arc<[u8]>,
    pub textures: [Option<TextureId>; MAX_SHADER_TEXTURES],
}

impl From<PaintShader> for Primitive {
    fn from(shader: PaintShader) -> Self {
        Primitive::Shader(shader)
    }
}
