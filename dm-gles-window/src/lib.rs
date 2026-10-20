// SPDX-License-Identifier: MIT OR Apache-2.0
//! DisplayManager window with an EGL/GLES2 surface.
//!
//! `display_create_window` takes the displayable ID as its fifth argument.
//! A DisplayManager context must hold the window before it can be shown.
//! Use `display-manager-control` to select that context.
//!
//! The library search path must contain the target graphics driver libraries,
//! including `libcgdrv.so`. Shaders use GLSL ES 1.00 and medium float precision.
//! All native functions are resolved at runtime with `dlopen` and `dlsym`.
//!
//! `Window::program` uses attribute `pos` at location 0 and `tc` at location 1.
//! `Window::draw_quad` supplies a full-window quad. Its texture origin is the
//! top left corner.

pub mod yuv;

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};

type Ptr = *mut c_void;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 0x0002;
const RTLD_GLOBAL: c_int = 0x0100;

macro_rules! api {
    ($(#[$m:meta])* $name:ident { $($f:ident : fn($($a:ty),*) $(-> $r:ty)?),* $(,)? }) => {
        $(#[$m])*
        #[allow(non_snake_case)]
        pub struct $name { $(pub $f: unsafe extern "C" fn($($a),*) $(-> $r)?),* }
        impl $name {
            fn load(h: Ptr) -> Result<Self, String> {
                Ok(Self { $($f: {
                    let n = concat!(stringify!($f), "\0");
                    let Ok(n) = CStr::from_bytes_with_nul(n.as_bytes()) else {
                        return Err(format!("bad name {}", stringify!($f)));
                    };
                    // SAFETY: h is a live dlopen handle, n is NUL-terminated.
                    let p = unsafe { dlsym(h, n.as_ptr()) };
                    if p.is_null() {
                        return Err(format!("missing {}", stringify!($f)));
                    }
                    // SAFETY: the library's function of that name has this
                    // signature (Khronos EGL 1.4 / GLES 2.0).
                    unsafe {
                        std::mem::transmute::<Ptr, unsafe extern "C" fn($($a),*) $(-> $r)?>(p)
                    }
                }),* })
            }
        }
    };
}

api!(
    /// The EGL entry points the window uses.
    Egl {
        eglGetDisplay: fn(Ptr) -> Ptr,
        eglInitialize: fn(Ptr, *mut c_int, *mut c_int) -> c_uint,
        eglChooseConfig: fn(Ptr, *const c_int, *mut Ptr, c_int, *mut c_int) -> c_uint,
        eglBindAPI: fn(c_uint) -> c_uint,
        eglCreateContext: fn(Ptr, Ptr, Ptr, *const c_int) -> Ptr,
        eglCreateWindowSurface: fn(Ptr, Ptr, Ptr, *const c_int) -> Ptr,
        eglMakeCurrent: fn(Ptr, Ptr, Ptr, Ptr) -> c_uint,
        eglSwapBuffers: fn(Ptr, Ptr) -> c_uint,
        eglGetError: fn() -> c_int,
    }
);

api!(
    /// The GLES2 entry points available to callers through [`Window::gl`].
    Gles {
        glCreateShader: fn(c_uint) -> c_uint,
        glShaderSource: fn(c_uint, c_int, *const *const c_char, *const c_int),
        glCompileShader: fn(c_uint),
        glGetShaderiv: fn(c_uint, c_uint, *mut c_int),
        glGetShaderInfoLog: fn(c_uint, c_int, *mut c_int, *mut c_char),
        glCreateProgram: fn() -> c_uint,
        glAttachShader: fn(c_uint, c_uint),
        glBindAttribLocation: fn(c_uint, c_uint, *const c_char),
        glLinkProgram: fn(c_uint),
        glGetProgramiv: fn(c_uint, c_uint, *mut c_int),
        glGetProgramInfoLog: fn(c_uint, c_int, *mut c_int, *mut c_char),
        glUseProgram: fn(c_uint),
        glGetUniformLocation: fn(c_uint, *const c_char) -> c_int,
        glUniform1i: fn(c_int, c_int),
        glUniform1f: fn(c_int, f32),
        glUniform2f: fn(c_int, f32, f32),
        glUniform4f: fn(c_int, f32, f32, f32, f32),
        glGenTextures: fn(c_int, *mut c_uint),
        glBindTexture: fn(c_uint, c_uint),
        glActiveTexture: fn(c_uint),
        glTexParameteri: fn(c_uint, c_uint, c_int),
        glTexImage2D: fn(c_uint, c_int, c_int, c_int, c_int, c_int, c_uint, c_uint, *const c_void),
        glTexSubImage2D: fn(c_uint, c_int, c_int, c_int, c_int, c_int, c_uint, c_uint, *const c_void),
        glPixelStorei: fn(c_uint, c_int),
        glVertexAttribPointer: fn(c_uint, c_int, c_uint, u8, c_int, *const c_void),
        glEnableVertexAttribArray: fn(c_uint),
        glDrawArrays: fn(c_uint, c_int, c_int),
        glViewport: fn(c_int, c_int, c_int, c_int),
        glClearColor: fn(f32, f32, f32, f32),
        glClear: fn(c_uint),
        glGetError: fn() -> c_uint,
    }
);

type DisplayInit = unsafe extern "C" fn(c_int, c_int);
type CreateWindow = unsafe extern "C" fn(Ptr, Ptr, c_int, c_int, c_int, *mut Ptr, *mut Ptr);

// EGL / GLES constants (Khronos headers).
const EGL_SURFACE_TYPE: c_int = 0x3033;
const EGL_WINDOW_BIT: c_int = 0x0004;
const EGL_RENDERABLE_TYPE: c_int = 0x3040;
const EGL_OPENGL_ES2_BIT: c_int = 0x0004;
const EGL_RED_SIZE: c_int = 0x3024;
const EGL_GREEN_SIZE: c_int = 0x3023;
const EGL_BLUE_SIZE: c_int = 0x3022;
const EGL_ALPHA_SIZE: c_int = 0x3021;
const EGL_DEPTH_SIZE: c_int = 0x3025;
const EGL_NONE: c_int = 0x3038;
const EGL_CONTEXT_CLIENT_VERSION: c_int = 0x3098;
const EGL_OPENGL_ES_API: c_uint = 0x30A0;
const GL_VERTEX_SHADER: c_uint = 0x8B31;
const GL_FRAGMENT_SHADER: c_uint = 0x8B30;
const GL_COMPILE_STATUS: c_uint = 0x8B81;
const GL_LINK_STATUS: c_uint = 0x8B82;
const GL_FLOAT: c_uint = 0x1406;
const GL_TRIANGLE_STRIP: c_uint = 0x0005;
const GL_COLOR_BUFFER_BIT: c_uint = 0x4000;
pub const GL_TEXTURE_2D: c_uint = 0x0DE1;
pub const GL_TEXTURE0: c_uint = 0x84C0;
pub const GL_TEXTURE_MIN_FILTER: c_uint = 0x2801;
pub const GL_TEXTURE_MAG_FILTER: c_uint = 0x2800;
pub const GL_TEXTURE_WRAP_S: c_uint = 0x2802;
pub const GL_TEXTURE_WRAP_T: c_uint = 0x2803;
pub const GL_LINEAR: c_int = 0x2601;
pub const GL_CLAMP_TO_EDGE: c_int = 0x812F;
pub const GL_LUMINANCE: c_uint = 0x1909;
pub const GL_UNSIGNED_BYTE: c_uint = 0x1401;
pub const GL_UNPACK_ALIGNMENT: c_uint = 0x0CF5;

/// Full-window quad: x, y, u, v; v = 0 is the picture's top row.
static QUAD: [f32; 16] = [
    -1.0, 1.0, 0.0, 0.0, //
    -1.0, -1.0, 0.0, 1.0, //
    1.0, 1.0, 1.0, 0.0, //
    1.0, -1.0, 1.0, 1.0,
];

/// The window, its EGL surface and a current GLES2 context. Use it from the
/// thread that created it.
pub struct Window {
    egl: Egl,
    /// The GLES2 functions, for callers that draw their own content.
    pub gl: Gles,
    dpy: Ptr,
    surface: Ptr,
    pub w: i32,
    pub h: i32,
}

fn open(path: &CStr) -> Result<Ptr, String> {
    // SAFETY: valid NUL-terminated path.
    let h = unsafe { dlopen(path.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    if h.is_null() {
        Err(format!("{} not loadable", path.to_string_lossy()))
    } else {
        Ok(h)
    }
}

impl Window {
    /// Create the window (claiming displayable `id`, RGB565), its EGL
    /// surface and a current ES2 context, and clear it to black.
    pub fn new(id: i32, w: i32, h: i32) -> Result<Self, String> {
        let di = open(c"libdisplayinit.so")?;
        let egl = Egl::load(open(c"libEGL.so")?)?;
        let gl = Gles::load(open(c"libGLESv2.so")?)?;
        // SAFETY: the resolved libdisplayinit symbols use the declared native signatures.
        unsafe {
            let init = dlsym(di, c"display_init".as_ptr());
            let create = dlsym(di, c"display_create_window".as_ptr());
            if create.is_null() {
                return Err("libdisplayinit lacks display_create_window".into());
            }
            let create = std::mem::transmute::<Ptr, CreateWindow>(create);
            if !init.is_null() {
                std::mem::transmute::<Ptr, DisplayInit>(init)(0, 0);
            }
            let dpy = (egl.eglGetDisplay)(std::ptr::null_mut());
            let (mut maj, mut min) = (0, 0);
            if dpy.is_null() || (egl.eglInitialize)(dpy, &mut maj, &mut min) == 0 {
                return Err(format!("eglInitialize failed 0x{:x}", (egl.eglGetError)()));
            }
            let attrs = [
                EGL_SURFACE_TYPE,
                EGL_WINDOW_BIT,
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_ES2_BIT,
                EGL_RED_SIZE,
                5,
                EGL_GREEN_SIZE,
                6,
                EGL_BLUE_SIZE,
                5,
                EGL_ALPHA_SIZE,
                0,
                EGL_DEPTH_SIZE,
                0,
                EGL_NONE,
            ];
            let mut config: Ptr = std::ptr::null_mut();
            let mut n = 0;
            if (egl.eglChooseConfig)(dpy, attrs.as_ptr(), &mut config, 1, &mut n) == 0 || n < 1 {
                return Err(format!("no RGB565 ES2 config 0x{:x}", (egl.eglGetError)()));
            }
            (egl.eglBindAPI)(EGL_OPENGL_ES_API);
            let ctx_attrs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
            let ctx = (egl.eglCreateContext)(dpy, config, std::ptr::null_mut(), ctx_attrs.as_ptr());
            if ctx.is_null() {
                return Err(format!(
                    "eglCreateContext failed 0x{:x}",
                    (egl.eglGetError)()
                ));
            }
            let mut native: Ptr = std::ptr::null_mut();
            let mut kd: Ptr = std::ptr::null_mut();
            create(dpy, config, w, h, id, &mut native, &mut kd);
            if native.is_null() {
                return Err(format!(
                    "display_create_window({w}x{h}, id {id}) gave no window"
                ));
            }
            let surface = (egl.eglCreateWindowSurface)(dpy, config, native, std::ptr::null());
            if surface.is_null() {
                return Err(format!(
                    "eglCreateWindowSurface failed 0x{:x}",
                    (egl.eglGetError)()
                ));
            }
            if (egl.eglMakeCurrent)(dpy, surface, surface, ctx) == 0 {
                return Err(format!("eglMakeCurrent failed 0x{:x}", (egl.eglGetError)()));
            }
            (gl.glViewport)(0, 0, w, h);
            let win = Window {
                egl,
                gl,
                dpy,
                surface,
                w,
                h,
            };
            win.clear();
            Ok(win)
        }
    }

    /// Present the back buffer.
    pub fn swap(&self) {
        // SAFETY: current context and surface.
        unsafe { (self.egl.eglSwapBuffers)(self.dpy, self.surface) };
    }

    /// Black, so the window never shows stale content when it goes up.
    pub fn clear(&self) {
        let g = &self.gl;
        // SAFETY: current context and surface.
        unsafe {
            (g.glClearColor)(0.0, 0.0, 0.0, 1.0);
            (g.glClear)(GL_COLOR_BUFFER_BIT);
        }
        self.swap();
    }

    fn shader(&self, kind: c_uint, src: &CStr) -> Result<c_uint, String> {
        let g = &self.gl;
        // SAFETY: current context; NUL-terminated source, length -1.
        unsafe {
            let s = (g.glCreateShader)(kind);
            let p = src.as_ptr();
            (g.glShaderSource)(s, 1, &p, std::ptr::null());
            (g.glCompileShader)(s);
            let mut ok = 0;
            (g.glGetShaderiv)(s, GL_COMPILE_STATUS, &mut ok);
            if ok == 0 {
                let mut buf = [0 as c_char; 512];
                let mut n = 0;
                (g.glGetShaderInfoLog)(s, buf.len() as c_int, &mut n, buf.as_mut_ptr());
                let msg = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
                return Err(format!("shader compile failed: {msg}"));
            }
            Ok(s)
        }
    }

    /// Compile and link a program from a vertex and a fragment shader, with
    /// `pos` bound to attribute 0 and `tc` to attribute 1 (see
    /// [`Window::draw_quad`]). The program is left in use.
    pub fn program(&self, vs: &CStr, fs: &CStr) -> Result<c_uint, String> {
        let vs = self.shader(GL_VERTEX_SHADER, vs)?;
        let fs = self.shader(GL_FRAGMENT_SHADER, fs)?;
        let g = &self.gl;
        // SAFETY: current context; valid shader objects.
        unsafe {
            let p = (g.glCreateProgram)();
            (g.glAttachShader)(p, vs);
            (g.glAttachShader)(p, fs);
            (g.glBindAttribLocation)(p, 0, c"pos".as_ptr());
            (g.glBindAttribLocation)(p, 1, c"tc".as_ptr());
            (g.glLinkProgram)(p);
            let mut ok = 0;
            (g.glGetProgramiv)(p, GL_LINK_STATUS, &mut ok);
            if ok == 0 {
                let mut buf = [0 as c_char; 512];
                let mut n = 0;
                (g.glGetProgramInfoLog)(p, buf.len() as c_int, &mut n, buf.as_mut_ptr());
                let msg = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
                return Err(format!("program link failed: {msg}"));
            }
            (g.glUseProgram)(p);
            Ok(p)
        }
    }

    /// The location of uniform `name` in program `p` (-1 if absent).
    pub fn uniform(&self, p: c_uint, name: &CStr) -> c_int {
        // SAFETY: current context; NUL-terminated name.
        unsafe { (self.gl.glGetUniformLocation)(p, name.as_ptr()) }
    }

    /// Draw the full-window quad with the program in use (attributes 0 and
    /// 1 as set up by [`Window::program`]). Does not swap. Returns the GL
    /// error code (0 = none).
    pub fn draw_quad(&self) -> u32 {
        let g = &self.gl;
        // SAFETY: current context; QUAD is static.
        unsafe {
            let q = QUAD.as_ptr();
            (g.glVertexAttribPointer)(0, 2, GL_FLOAT, 0, 16, q.cast());
            (g.glVertexAttribPointer)(1, 2, GL_FLOAT, 0, 16, q.add(2).cast());
            (g.glEnableVertexAttribArray)(0);
            (g.glEnableVertexAttribArray)(1);
            (g.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
            (g.glGetError)()
        }
    }

    /// A new texture on unit `unit`, linear filtering, clamped edges; left
    /// bound.
    pub fn texture(&self, unit: u32) -> c_uint {
        let g = &self.gl;
        let mut t = 0;
        // SAFETY: current context.
        unsafe {
            (g.glGenTextures)(1, &mut t);
            (g.glActiveTexture)(GL_TEXTURE0 + unit);
            (g.glBindTexture)(GL_TEXTURE_2D, t);
            (g.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (g.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            (g.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (g.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            (g.glPixelStorei)(GL_UNPACK_ALIGNMENT, 1);
        }
        t
    }

    /// Upload an 8-bit single-channel image (`w` x `h` bytes, rows top
    /// first) into texture `t` on unit `unit`, (re)allocating it.
    pub fn upload_luminance(&self, unit: u32, t: c_uint, w: i32, h: i32, pixels: &[u8]) {
        if pixels.len() < (w.max(0) as usize) * (h.max(0) as usize) {
            return;
        }
        let g = &self.gl;
        // SAFETY: current context; pixels holds w * h bytes (checked above).
        unsafe {
            (g.glActiveTexture)(GL_TEXTURE0 + unit);
            (g.glBindTexture)(GL_TEXTURE_2D, t);
            (g.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_LUMINANCE as c_int,
                w,
                h,
                0,
                GL_LUMINANCE,
                GL_UNSIGNED_BYTE,
                pixels.as_ptr().cast(),
            );
        }
    }
}
