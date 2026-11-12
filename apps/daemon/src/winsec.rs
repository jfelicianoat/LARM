//! Named pipes con DACL restringida al SID del usuario (P009, contrato 03).
//!
//! **Única excepción a la prohibición de `unsafe` del workspace (ADR-0009).**
//! Windows solo permite fijar la DACL de un pipe al crearlo, pasando un
//! `SECURITY_ATTRIBUTES`, y la única vía es FFI: SDDL → descriptor con
//! `ConvertStringSecurityDescriptorToSecurityDescriptorW` y creación con
//! `ServerOptions::create_with_security_attributes_raw`. El `unsafe` se limita
//! a esas dos llamadas y a liberar el descriptor; todo lo demás es seguro.
#![allow(unsafe_code)]

use std::io;
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

/// DACL protegida (sin herencia): control total solo para `sid` y SYSTEM.
/// Administradores y el resto de usuarios no pueden conectar.
///
/// # Errors
/// `InvalidInput` si `sid` no tiene forma de SID (`S-1-...`).
pub fn sddl_for(sid: &str) -> io::Result<String> {
    let valid = sid.starts_with("S-1-")
        && sid.len() <= 184
        && sid[2..].bytes().all(|b| b.is_ascii_digit() || b == b'-');
    if !valid {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "SID no válido"));
    }
    Ok(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"))
}

/// Descriptor de seguridad liberado al salir de ámbito.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: el puntero lo reservó ConvertStringSecurityDescriptorToSecurityDescriptorW
        // con LocalAlloc y solo se libera aquí, una vez.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn descriptor(sddl: &str) -> io::Result<Descriptor> {
    let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `wide` termina en NUL y vive durante la llamada; `sd` es un
    // puntero de salida válido; el tamaño de salida es opcional (nulo).
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || sd.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(Descriptor(sd))
}

/// Crea una instancia del pipe `name` que solo `sid` (y SYSTEM) pueden abrir,
/// rechaza clientes remotos y trabaja en modo byte.
///
/// # Errors
/// SID o SDDL inválidos, o el error de `CreateNamedPipeW` (p. ej. el nombre ya
/// está en uso con `first`).
pub fn create_pipe(name: &str, sid: &str, first: bool, buffer: u32) -> io::Result<NamedPipeServer> {
    let sd = descriptor(&sddl_for(sid)?)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte)
        .in_buffer_size(buffer)
        .out_buffer_size(buffer);
    // SAFETY: `attributes` y el descriptor que apunta siguen vivos durante la
    // llamada (el descriptor se libera al soltar `sd`, después).
    unsafe {
        options.create_with_security_attributes_raw(
            name,
            std::ptr::addr_of_mut!(attributes).cast::<std::ffi::c_void>(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sddl_solo_para_sids_validos() {
        assert_eq!(
            sddl_for("S-1-5-21-1-2-3-1001").ok().as_deref(),
            Some("D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)")
        );
        // un SID con caracteres de SDDL podría inyectar ACEs: se rechaza antes
        for bad in ["", "S-1-5)(A;;GA;;;WD", "BA", "S-1-5-21 1"] {
            assert!(sddl_for(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn descriptor_invalido_falla_sin_crear_nada() {
        assert!(descriptor("esto no es SDDL").is_err());
    }
}
