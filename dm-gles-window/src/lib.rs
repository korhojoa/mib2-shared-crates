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

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::rc::{Rc, Weak};

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
            #[cfg(test)]
            fn test_stub() -> Self {
                Self { $($f: {
                    unsafe extern "C" fn stub($(_: $a),*) $(-> $r)? {
                        Default::default()
                    }
                    stub
                }),* }
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
        eglGetCurrentContext: fn() -> Ptr,
        eglSwapBuffers: fn(Ptr, Ptr) -> c_uint,
        eglGetError: fn() -> c_int,
        eglDestroySurface: fn(Ptr, Ptr) -> c_uint,
        eglDestroyContext: fn(Ptr, Ptr) -> c_uint,
    }
);

api!(
    /// The GLES2 entry points available to callers through [`Window::gl`].
    Gles {
        glCreateShader: fn(c_uint) -> c_uint,
        glDeleteShader: fn(c_uint),
        glShaderSource: fn(c_uint, c_int, *const *const c_char, *const c_int),
        glCompileShader: fn(c_uint),
        glGetShaderiv: fn(c_uint, c_uint, *mut c_int),
        glGetShaderInfoLog: fn(c_uint, c_int, *mut c_int, *mut c_char),
        glCreateProgram: fn() -> c_uint,
        glAttachShader: fn(c_uint, c_uint),
        glDetachShader: fn(c_uint, c_uint),
        glDeleteProgram: fn(c_uint),
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

struct Shader<'a> {
    id: c_uint,
    gl: &'a Gles,
}
impl Drop for Shader<'_> {
    fn drop(&mut self) {
        // SAFETY: this temporary stays inside compilation in the current context.
        unsafe {
            (self.gl.glDeleteShader)(self.id);
        }
    }
}
struct Programs {
    ids: RefCell<Vec<c_uint>>,
    current: unsafe extern "C" fn() -> Ptr,
    delete: unsafe extern "C" fn(c_uint),
    context: Ptr,
    live: Cell<bool>,
}
impl Programs {
    fn new(egl: &Egl, gl: &Gles, context: Ptr) -> Rc<Self> {
        Rc::new(Self {
            ids: RefCell::new(Vec::new()),
            current: egl.eglGetCurrentContext,
            delete: gl.glDeleteProgram,
            context,
            live: Cell::new(true),
        })
    }
    fn release(&self, id: c_uint) {
        // SAFETY: the function table is process-scoped. Never delete in another context.
        if self.live.get() && unsafe { (self.current)() } == self.context {
            let mut ids = self.ids.borrow_mut();
            if let Some(index) = ids.iter().position(|p| *p == id) {
                ids.swap_remove(index);
                // SAFETY: this ID belongs to the current live context.
                unsafe {
                    (self.delete)(id);
                }
            }
        }
    }
}
struct Program {
    id: c_uint,
    owner: Weak<Programs>,
}
impl Drop for Program {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.release(self.id);
        }
    }
}

/// The window, its EGL surface and a current GLES2 context. Use it from the
/// thread that created it. Drop releases its EGL surface and context.
/// Native display windows and library handles remain process-scoped.
pub struct Window {
    programs: Rc<Programs>,
    egl: Egl,
    /// The GLES2 functions, for callers that draw their own content.
    pub gl: Gles,
    dpy: Ptr,
    surface: Ptr,
    context: Ptr,
    pub w: i32,
    pub h: i32,
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: these are this window's handles, released on its creating thread.
        // A partially constructed window can own a context without a surface.
        unsafe {
            if !self.context.is_null() && (self.egl.eglGetCurrentContext)() == self.context {
                for program in self.programs.ids.borrow_mut().drain(..) {
                    (self.gl.glDeleteProgram)(program);
                }
                (self.egl.eglMakeCurrent)(
                    self.dpy,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
            self.programs.live.set(false);
            if !self.surface.is_null() {
                (self.egl.eglDestroySurface)(self.dpy, self.surface);
            }
            if !self.context.is_null() {
                (self.egl.eglDestroyContext)(self.dpy, self.context);
            }
        }
    }
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
    #[cfg(test)]
    fn test_window() -> Self {
        Self {
            programs: Programs::new(&Egl::test_stub(), &Gles::test_stub(), std::ptr::null_mut()),
            egl: Egl::test_stub(),
            gl: Gles::test_stub(),
            dpy: std::ptr::null_mut(),
            surface: std::ptr::null_mut(),
            context: std::ptr::null_mut(),
            w: 1,
            h: 1,
        }
    }

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
            let mut win = Window {
                programs: Programs::new(&egl, &gl, ctx),
                egl,
                gl,
                dpy,
                surface: std::ptr::null_mut(),
                context: ctx,
                w,
                h,
            };
            let mut native: Ptr = std::ptr::null_mut();
            let mut kd: Ptr = std::ptr::null_mut();
            create(dpy, config, w, h, id, &mut native, &mut kd);
            if native.is_null() {
                return Err(format!(
                    "display_create_window({w}x{h}, id {id}) gave no window"
                ));
            }
            win.surface = (win.egl.eglCreateWindowSurface)(dpy, config, native, std::ptr::null());
            if win.surface.is_null() {
                return Err(format!(
                    "eglCreateWindowSurface failed 0x{:x}",
                    (win.egl.eglGetError)()
                ));
            }
            if (win.egl.eglMakeCurrent)(dpy, win.surface, win.surface, ctx) == 0 {
                return Err(format!(
                    "eglMakeCurrent failed 0x{:x}",
                    (win.egl.eglGetError)()
                ));
            }
            (win.gl.glViewport)(0, 0, w, h);
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

    fn shader(&self, kind: c_uint, src: &CStr) -> Result<Shader<'_>, String> {
        let g = &self.gl;
        // SAFETY: current context; NUL-terminated source, length -1.
        unsafe {
            let id = (g.glCreateShader)(kind);
            if id == 0 {
                return Err("shader allocation failed".into());
            }
            let shader = Shader { id, gl: g };
            let s = shader.id;
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
            Ok(shader)
        }
    }

    /// Compile and link a program from a vertex and a fragment shader, with
    /// `pos` bound to attribute 0 and `tc` to attribute 1 (see
    /// [`Window::draw_quad`]). The program is left in use and owned by this window.
    /// Use [`Window::delete_program`] to release it before window destruction.
    pub fn program(&self, vs: &CStr, fs: &CStr) -> Result<c_uint, String> {
        let vs = self.shader(GL_VERTEX_SHADER, vs)?;
        let fs = self.shader(GL_FRAGMENT_SHADER, fs)?;
        let g = &self.gl;
        // SAFETY: current context; valid shader objects.
        unsafe {
            let p = (g.glCreateProgram)();
            if p == 0 {
                return Err("program allocation failed".into());
            }
            (g.glAttachShader)(p, vs.id);
            (g.glAttachShader)(p, fs.id);
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
                (g.glDeleteProgram)(p);
                return Err(format!("program link failed: {msg}"));
            }
            (g.glDetachShader)(p, vs.id);
            (g.glDetachShader)(p, fs.id);
            self.programs.ids.borrow_mut().push(p);
            (g.glUseProgram)(p);
            Ok(p)
        }
    }

    /// Release an unused program while this window's context is current.
    /// Otherwise, context destruction releases the program.
    pub fn delete_program(&self, program: c_uint) {
        self.programs.release(program);
    }

    fn own_program(&self, id: c_uint) -> Program {
        Program {
            id,
            owner: Rc::downgrade(&self.programs),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    static RELEASED: Mutex<Vec<&str>> = Mutex::new(Vec::new());
    unsafe extern "C" fn current_context() -> Ptr {
        core::ptr::dangling_mut()
    }
    unsafe extern "C" fn unbind(_: Ptr, _: Ptr, _: Ptr, _: Ptr) -> c_uint {
        RELEASED.lock().unwrap().push("unbind");
        1
    }
    unsafe extern "C" fn surface(_: Ptr, _: Ptr) -> c_uint {
        RELEASED.lock().unwrap().push("surface");
        1
    }
    unsafe extern "C" fn context(_: Ptr, _: Ptr) -> c_uint {
        RELEASED.lock().unwrap().push("context");
        1
    }
    #[test]
    fn dropping_window_releases_its_egl_surface_and_context() {
        let mut egl = Egl::test_stub();
        egl.eglMakeCurrent = unbind;
        egl.eglGetCurrentContext = current_context;
        egl.eglDestroySurface = surface;
        egl.eglDestroyContext = context;
        let window = Window {
            programs: Programs::new(&egl, &Gles::test_stub(), core::ptr::dangling_mut()),
            egl,
            gl: Gles::test_stub(),
            dpy: core::ptr::dangling_mut(),
            surface: core::ptr::dangling_mut(),
            context: core::ptr::dangling_mut(),
            w: 1,
            h: 1,
        };
        drop(window);
        assert_eq!(*RELEASED.lock().unwrap(), ["unbind", "surface", "context"]);
        RELEASED.lock().unwrap().clear();
        let mut egl = Egl::test_stub();
        egl.eglDestroyContext = context;
        // No surface and no current context: the context-only failure path.
        let partial = Window {
            programs: Programs::new(&egl, &Gles::test_stub(), core::ptr::dangling_mut()),
            egl,
            gl: Gles::test_stub(),
            dpy: core::ptr::dangling_mut(),
            surface: core::ptr::null_mut(),
            context: core::ptr::dangling_mut(),
            w: 1,
            h: 1,
        };
        drop(partial);
        assert_eq!(*RELEASED.lock().unwrap(), ["context"]);
    }
}

#[cfg(test)]
mod program_tests {
    use super::*;
    #[derive(Default)]
    struct State {
        next: u32,
        fail: u32,
        shaders: Vec<u32>,
        programs: Vec<u32>,
        detached: Vec<(u32, u32)>,
        events: Vec<&'static str>,
    }
    thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
    unsafe extern "C" fn shader(_: u32) -> u32 {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.next += 1;
            if s.fail == 4 && s.next == 2 {
                0
            } else {
                s.next
            }
        })
    }
    unsafe extern "C" fn shader_status(id: u32, _: u32, out: *mut i32) {
        let ok = STATE.with(|s| {
            let s = s.borrow();
            !(s.fail == 1 && id == 1 || s.fail == 2 && id == 2)
        });
        // SAFETY: the library supplies its status output pointer.
        unsafe {
            *out = i32::from(ok);
        }
    }
    unsafe extern "C" fn program() -> u32 {
        STATE.with(|s| if s.borrow().fail == 5 { 0 } else { 3 })
    }
    unsafe extern "C" fn program_status(_: u32, _: u32, out: *mut i32) {
        let ok = STATE.with(|s| s.borrow().fail != 3);
        // SAFETY: the library supplies its status output pointer.
        unsafe {
            *out = i32::from(ok);
        }
    }
    unsafe extern "C" fn delete_shader(id: u32) {
        STATE.with(|s| s.borrow_mut().shaders.push(id));
    }
    unsafe extern "C" fn delete_program(id: u32) {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.programs.push(id);
            s.events.push("program");
        });
    }
    unsafe extern "C" fn detach(p: u32, s: u32) {
        STATE.with(|state| state.borrow_mut().detached.push((p, s)));
    }
    unsafe extern "C" fn current() -> Ptr {
        core::ptr::dangling_mut()
    }
    unsafe extern "C" fn unbind(_: Ptr, _: Ptr, _: Ptr, _: Ptr) -> u32 {
        STATE.with(|s| s.borrow_mut().events.push("unbind"));
        1
    }
    unsafe extern "C" fn destroy(_: Ptr, _: Ptr) -> u32 {
        STATE.with(|s| s.borrow_mut().events.push("context"));
        1
    }
    fn window() -> Window {
        let mut window = Window::test_window();
        window.gl.glCreateShader = shader;
        window.gl.glGetShaderiv = shader_status;
        window.gl.glDeleteShader = delete_shader;
        window.gl.glCreateProgram = program;
        window.gl.glGetProgramiv = program_status;
        window.gl.glDeleteProgram = delete_program;
        window.gl.glDetachShader = detach;
        window.programs = Programs::new(&window.egl, &window.gl, window.context);
        window
    }
    #[test]
    fn compilation_and_link_failures_release_every_allocation() {
        for fail in 0..=5 {
            STATE.with(|s| {
                *s.borrow_mut() = State {
                    fail,
                    ..State::default()
                }
            });
            let window = window();
            let result = window.program(c"vertex", c"fragment");
            assert_eq!(result.is_ok(), fail == 0);
            if let Ok(id) = result {
                window.delete_program(id);
                window.delete_program(id);
            }
            STATE.with(|s| {
                let s = s.borrow();
                let mut shaders = s.shaders.clone();
                shaders.sort_unstable();
                assert_eq!(
                    shaders,
                    if fail == 1 || fail == 4 {
                        vec![1]
                    } else {
                        vec![1, 2]
                    },
                    "case {fail}"
                );
                assert_eq!(
                    s.programs,
                    if fail == 0 || fail == 3 {
                        vec![3]
                    } else {
                        vec![]
                    },
                    "case {fail}"
                );
                assert_eq!(s.detached.len(), if fail == 0 { 2 } else { 0 });
            });
        }
    }
    #[test]
    fn dropping_the_renderer_releases_its_program_in_the_live_context() {
        STATE.with(|s| *s.borrow_mut() = State::default());
        let window = window();
        let renderer = yuv::Yuv420::new(&window).unwrap();
        assert_eq!(window.programs.ids.borrow().len(), 1);
        drop(renderer);
        assert!(window.programs.ids.borrow().is_empty());
        STATE.with(|s| assert_eq!(s.borrow().programs, [3]));
    }
    #[test]
    fn remaining_programs_are_deleted_before_unbinding_the_context() {
        STATE.with(|s| *s.borrow_mut() = State::default());
        let mut window = window();
        window.context = core::ptr::dangling_mut();
        window.egl.eglGetCurrentContext = current;
        window.egl.eglMakeCurrent = unbind;
        window.egl.eglDestroyContext = destroy;
        window.programs = Programs::new(&window.egl, &window.gl, window.context);
        let id = window.program(c"vertex", c"fragment").unwrap();
        let late_owner = window.own_program(id);
        drop(window);
        drop(late_owner);
        STATE.with(|s| assert_eq!(s.borrow().events, ["program", "unbind", "context"]));
    }
}
