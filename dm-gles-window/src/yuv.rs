// SPDX-License-Identifier: MIT OR Apache-2.0
//! Draws YUV 4:2:0 pictures (ffmpeg's planar layout) into a [`Window`],
//! scaled to the window: three LUMINANCE textures at full linesize and a
//! BT.601 limited-range shader.

use crate::{Window, GL_LUMINANCE, GL_TEXTURE0, GL_TEXTURE_2D, GL_UNSIGNED_BYTE};
use std::ffi::{c_int, c_uint, CStr};

const VS: &CStr = c"#version 100
attribute vec2 pos;
attribute vec2 tc;
varying vec2 uv;
void main() { gl_Position = vec4(pos, 0.0, 1.0); uv = tc; }
";

// BT.601 limited range, the usual H.264 default. Each plane is uploaded at
// its full linesize (GLES2 has no row-length unpack), so each gets its own
// horizontal scale (visible width / linesize).
const FS: &CStr = c"#version 100
precision mediump float;
varying vec2 uv;
uniform sampler2D ty;
uniform sampler2D tu;
uniform sampler2D tv;
uniform float sy;
uniform float su;
uniform float sv;
void main() {
    float y = 1.1643 * (texture2D(ty, vec2(uv.x * sy, uv.y)).r - 0.0625);
    float u = texture2D(tu, vec2(uv.x * su, uv.y)).r - 0.5;
    float v = texture2D(tv, vec2(uv.x * sv, uv.y)).r - 0.5;
    gl_FragColor = vec4(y + 1.5958 * v, y - 0.39173 * u - 0.8129 * v, y + 2.017 * u, 1.0);
}
";

/// The YUV program and its three plane textures (units 0..2).
pub struct Yuv420 {
    prog: crate::Program,
    tex: [c_uint; 3],
    /// Texture sizes allocated so far: (linesize, rows) per plane.
    tex_size: [(i32, i32); 3],
    u_sy: c_int,
    u_su: c_int,
    u_sv: c_int,
}

impl Yuv420 {
    pub fn new(win: &Window) -> Result<Self, String> {
        let prog = win.program(VS, FS)?;
        let g = &win.gl;
        // SAFETY: current context; program in use.
        unsafe {
            for (i, name) in [c"ty", c"tu", c"tv"].iter().enumerate() {
                (g.glUniform1i)(win.uniform(prog, name), i as c_int);
            }
        }
        let tex = [win.texture(0), win.texture(1), win.texture(2)];
        Ok(Self {
            prog: win.own_program(prog),
            tex,
            tex_size: [(0, 0); 3],
            u_sy: win.uniform(prog, c"sy"),
            u_su: win.uniform(prog, c"su"),
            u_sv: win.uniform(prog, c"sv"),
        })
    }

    /// Draw one picture (planes, linesizes, visible size) and swap. Each
    /// plane must hold `lines[i]` x rows bytes (rows = h for Y, (h+1)/2 for
    /// U and V). Returns the GL error code (0 = none).
    ///
    /// A caller must not dereference arbitrary planes through safe code.
    ///
    /// ```compile_fail
    /// use dm_gles_window::{Window, yuv::Yuv420};
    /// fn render(renderer: &mut Yuv420, window: &Window) {
    ///     renderer.draw(window, [core::ptr::null(); 3], [1; 3], 1, 1);
    /// }
    /// ```
    /// # Safety
    /// Each plane must remain readable for `lines[i] * rows[i]` bytes during this call.
    /// Dimensions and strides must be positive. `w + 1` and `h + 1` must fit in `i32`.
    /// The current EGL context must be the context used by `new`.
    pub unsafe fn draw(
        &mut self,
        win: &Window,
        planes: [*const u8; 3],
        lines: [i32; 3],
        w: i32,
        h: i32,
    ) -> u32 {
        let g = &win.gl;
        let rows = [h, (h + 1) / 2, (h + 1) / 2];
        // SAFETY: current context; each plane holds lines[i] * rows[i] bytes
        // (the caller's contract, ffmpeg's frame layout).
        unsafe {
            (g.glUseProgram)(self.prog.id);
            for i in 0..3 {
                (g.glActiveTexture)(GL_TEXTURE0 + i as c_uint);
                (g.glBindTexture)(GL_TEXTURE_2D, self.tex[i]);
                if self.tex_size[i] != (lines[i], rows[i]) {
                    (g.glTexImage2D)(
                        GL_TEXTURE_2D,
                        0,
                        GL_LUMINANCE as c_int,
                        lines[i],
                        rows[i],
                        0,
                        GL_LUMINANCE,
                        GL_UNSIGNED_BYTE,
                        planes[i].cast(),
                    );
                    self.tex_size[i] = (lines[i], rows[i]);
                } else {
                    (g.glTexSubImage2D)(
                        GL_TEXTURE_2D,
                        0,
                        0,
                        0,
                        lines[i],
                        rows[i],
                        GL_LUMINANCE,
                        GL_UNSIGNED_BYTE,
                        planes[i].cast(),
                    );
                }
            }
            (g.glUniform1f)(self.u_sy, w as f32 / lines[0].max(1) as f32);
            (g.glUniform1f)(self.u_su, ((w + 1) / 2) as f32 / lines[1].max(1) as f32);
        }
        // SAFETY: the same current context and program are still in use.
        unsafe {
            (g.glUniform1f)(self.u_sv, ((w + 1) / 2) as f32 / lines[2].max(1) as f32);
        }
        let e = win.draw_quad();
        win.swap();
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static SCALES: std::sync::Mutex<Vec<(i32, f32)>> = std::sync::Mutex::new(Vec::new());
    unsafe extern "C" fn scale(location: i32, value: f32) {
        SCALES.lock().unwrap().push((location, value));
    }
    #[test]
    fn draw_uses_each_planes_own_stride() {
        let mut win = Window::test_window();
        win.gl.glUniform1f = scale;
        let mut renderer = Yuv420 {
            prog: crate::Program {
                id: 1,
                owner: std::rc::Weak::new(),
            },
            tex: [1, 2, 3],
            tex_size: [(0, 0); 3],
            u_sy: 0,
            u_su: 1,
            u_sv: 2,
        };
        let pixels = [0u8; 16];
        // SAFETY: each owned plane exceeds its stride times the row count; GL calls are stubs.
        assert_eq!(
            unsafe { renderer.draw(&win, [pixels.as_ptr(); 3], [8, 4, 8], 5, 2) },
            0
        );
        assert_eq!(*SCALES.lock().unwrap(), [(0, 0.625), (1, 0.75), (2, 0.375)]);
    }
}
