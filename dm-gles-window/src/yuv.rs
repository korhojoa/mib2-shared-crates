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
uniform float sc;
void main() {
    float y = 1.1643 * (texture2D(ty, vec2(uv.x * sy, uv.y)).r - 0.0625);
    float u = texture2D(tu, vec2(uv.x * sc, uv.y)).r - 0.5;
    float v = texture2D(tv, vec2(uv.x * sc, uv.y)).r - 0.5;
    gl_FragColor = vec4(y + 1.5958 * v, y - 0.39173 * u - 0.8129 * v, y + 2.017 * u, 1.0);
}
";

/// The YUV program and its three plane textures (units 0..2).
pub struct Yuv420 {
    prog: c_uint,
    tex: [c_uint; 3],
    /// Texture sizes allocated so far: (linesize, rows) per plane.
    tex_size: [(i32, i32); 3],
    u_sy: c_int,
    u_sc: c_int,
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
            prog,
            tex,
            tex_size: [(0, 0); 3],
            u_sy: win.uniform(prog, c"sy"),
            u_sc: win.uniform(prog, c"sc"),
        })
    }

    /// Draw one picture (planes, linesizes, visible size) and swap. Each
    /// plane must hold `lines[i]` x rows bytes (rows = h for Y, (h+1)/2 for
    /// U and V). Returns the GL error code (0 = none).
    pub fn draw(
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
            (g.glUseProgram)(self.prog);
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
            (g.glUniform1f)(self.u_sc, ((w + 1) / 2) as f32 / lines[1].max(1) as f32);
        }
        let e = win.draw_quad();
        win.swap();
        e
    }
}
