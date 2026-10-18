//! Small owned adapters for the parts of libmpv's node API missing in libmpv2.
use std::ffi::{CStr, CString};

use anyhow::{bail, Result};
use libmpv2::{mpv_format as format, Mpv};
use libmpv2_sys as sys;
use serde_json::Value;

fn check(code: i32) -> Result<()> {
    if code < 0 {
        // SAFETY: mpv returns a static, null-terminated description for every code.
        let description = unsafe { CStr::from_ptr(sys::mpv_error_string(code)) };
        bail!("{}", description.to_string_lossy());
    }
    Ok(())
}

pub fn request_errors(mpv: &Mpv) -> Result<()> {
    // SAFETY: live client handle and a static C string; no retained pointers.
    check(unsafe { sys::mpv_request_log_messages(mpv.ctx.as_ptr(), c"error".as_ptr()) })
}

pub fn set_strings(mpv: &Mpv, name: &str, values: &[String]) -> Result<()> {
    let name = CString::new(name)?;
    let strings = values
        .iter()
        .map(|v| CString::new(v.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut nodes: Vec<_> = strings
        .iter()
        .map(|s| sys::mpv_node {
            format: format::String,
            u: sys::mpv_node__bindgen_ty_1 {
                string: s.as_ptr().cast_mut(),
            },
        })
        .collect();
    let mut list = sys::mpv_node_list {
        num: nodes.len().try_into()?,
        values: nodes.as_mut_ptr(),
        keys: std::ptr::null_mut(),
    };
    let mut node = sys::mpv_node {
        format: format::Array,
        u: sys::mpv_node__bindgen_ty_1 { list: &mut list },
    };
    // SAFETY: every referenced allocation remains alive for this synchronous call.
    // mpv copies the array and strings; it neither frees nor retains our pointers.
    check(unsafe {
        sys::mpv_set_property(
            mpv.ctx.as_ptr(),
            name.as_ptr(),
            format::Node,
            (&mut node as *mut sys::mpv_node).cast(),
        )
    })
}

struct OwnedNode(sys::mpv_node);
impl Drop for OwnedNode {
    fn drop(&mut self) {
        // SAFETY: this node was initialized by a successful mpv_get_property call.
        unsafe { sys::mpv_free_node_contents(&mut self.0) };
    }
}

pub fn get(mpv: &Mpv, name: &str) -> Result<Value> {
    let name = CString::new(name)?;
    let mut node = std::mem::MaybeUninit::<sys::mpv_node>::uninit();
    // SAFETY: writable output storage, valid handle/name. Only read on success.
    check(unsafe {
        sys::mpv_get_property(
            mpv.ctx.as_ptr(),
            name.as_ptr(),
            format::Node,
            node.as_mut_ptr().cast(),
        )
    })?;
    // SAFETY: success initializes the node and transfers ownership to the caller.
    let node = OwnedNode(unsafe { node.assume_init() });
    // SAFETY: the entire tree remains valid until OwnedNode is dropped.
    Ok(unsafe { copy_node(&node.0) })
}

/// Copy libmpv-owned values before freeing their node tree.
unsafe fn copy_node(node: &sys::mpv_node) -> Value {
    // SAFETY: only access the union member indicated by the format tag. mpv
    // guarantees valid strings/list entries; empty lists may have null pointers.
    unsafe {
        match node.format {
            format::String => CStr::from_ptr(node.u.string)
                .to_string_lossy()
                .into_owned()
                .into(),
            format::Flag => (node.u.flag != 0).into(),
            format::Int64 => node.u.int64.into(),
            format::Double => node.u.double_.into(),
            format::Array | format::Map => {
                let list = &*node.u.list;
                let values = (0..list.num as usize).map(|i| copy_node(&*list.values.add(i)));
                if node.format == format::Array {
                    Value::Array(values.collect())
                } else {
                    Value::Object(
                        values
                            .enumerate()
                            .map(|(i, v)| {
                                (
                                    CStr::from_ptr(*list.keys.add(i))
                                        .to_string_lossy()
                                        .into_owned(),
                                    v,
                                )
                            })
                            .collect(),
                    )
                }
            }
            _ => Value::Null,
        }
    }
}

pub fn strings(mpv: &Mpv, name: &str) -> Result<Vec<String>> {
    let node = get(mpv, name)?;
    let Some(array) = node.as_array() else {
        bail!("{name} is not a list");
    };
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(String::from)
                .ok_or_else(|| anyhow::anyhow!("{name} contains a non-string"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_shader_list_round_trips_special_paths_and_empty_list() {
        let mpv = Mpv::with_initializer(|init| {
            init.set_option("config", "no")?;
            init.set_option("vo", "null")
        })
        .unwrap();
        let paths = vec!["/tmp/a:b, c\\d.glsl".into(), "C:\\日本語\\a;b.glsl".into()];
        set_strings(&mpv, "glsl-shaders", &paths).unwrap();
        assert_eq!(strings(&mpv, "glsl-shaders").unwrap(), paths);
        assert!(set_strings(&mpv, "glsl-shaders", &["bad\0path".into()]).is_err());
        assert_eq!(strings(&mpv, "glsl-shaders").unwrap(), paths);
        set_strings(&mpv, "glsl-shaders", &[]).unwrap();
        assert!(strings(&mpv, "glsl-shaders").unwrap().is_empty());
    }
}
