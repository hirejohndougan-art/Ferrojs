//! String cells and the atom table (ARCHITECTURE.md §5.4, D-038).
//!
//! Layout: header (kind, flags in bits 8..16, length in UTF-16 code units
//! in bits 32..64), then the units: one byte each if every unit is below
//! 256 (Latin-1), else two bytes each, little-endian. Strings are
//! immutable. Atoms are strings interned in [`Tables`], so two atoms with
//! the same contents are the same cell and property keys compare by
//! reference.

use ferrojs_core::gc::Scope;
use ferrojs_core::{AllocError, Gc, NoGc, Unbound};

use crate::{CellKind, Tables};

/// Type marker for string cells.
pub struct JsString;

/// Flag: units are stored as two bytes.
const TWO_BYTE: u64 = 1 << 8;
/// Flag: the string is an interned atom.
const ATOM: u64 = 1 << 9;
const DATA: u32 = 8;

/// Allocates a string with the given UTF-16 code units.
pub fn alloc_units(s: &mut Scope<'_>, units: &[u16]) -> Result<Unbound<JsString>, AllocError> {
    alloc_with_flags(s, units, 0)
}

/// Allocates a string from Rust text (converted to UTF-16).
pub fn alloc_str(s: &mut Scope<'_>, text: &str) -> Result<Unbound<JsString>, AllocError> {
    let units: Vec<u16> = text.encode_utf16().collect();
    alloc_units(s, &units)
}

fn alloc_with_flags(
    s: &mut Scope<'_>,
    units: &[u16],
    flags: u64,
) -> Result<Unbound<JsString>, AllocError> {
    let len = u32::try_from(units.len()).map_err(|_| AllocError::BadSize(units.len() as u64))?;
    let two_byte = units.iter().any(|&u| u > 0xff);
    let too_big = || AllocError::BadSize(units.len() as u64 * 2);
    let bytes = if two_byte {
        len.checked_mul(2).ok_or_else(too_big)?
    } else {
        len
    };
    let size = DATA.checked_add(bytes).ok_or_else(too_big)?;
    let cell = s.alloc::<JsString>(size)?;
    let nogc = s.nogc();
    let g = cell.bind(nogc);
    let mut header = CellKind::String as u64 | flags | (len as u64) << 32;
    if two_byte {
        header |= TWO_BYTE;
        let data: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        nogc.write_bytes(g, DATA, &data);
    } else {
        let data: Vec<u8> = units.iter().map(|&u| u as u8).collect();
        nogc.write_bytes(g, DATA, &data);
    }
    nogc.write_u64(g, 0, header);
    Ok(g.unbind())
}

/// The length in UTF-16 code units.
pub fn len(nogc: NoGc<'_>, s: Gc<'_, JsString>) -> u32 {
    (header(nogc, s) >> 32) as u32
}

fn header(nogc: NoGc<'_>, s: Gc<'_, JsString>) -> u64 {
    let h = nogc.read_u64(s, 0);
    debug_assert_eq!(h as u8, CellKind::String as u8, "not a string cell");
    h
}

/// The string's UTF-16 code units.
pub fn units(nogc: NoGc<'_>, s: Gc<'_, JsString>) -> Vec<u16> {
    let _ = header(nogc, s);
    units_at(nogc.cage(), s.offset())
}

/// The code units of the string cell at `offset`, read straight from the
/// cage: for string references held as value bits. The caller vouches
/// that `offset` is a live string cell (a wrong one panics or reads
/// garbage; it can't break memory safety).
pub fn units_at(cage: &ferrojs_core::Cage, offset: u32) -> Vec<u16> {
    let h = cage.read_u64(offset);
    assert_eq!(h as u8, CellKind::String as u8, "not a string cell");
    let len = (h >> 32) as usize;
    if h & TWO_BYTE != 0 {
        let mut bytes = vec![0u8; 2 * len];
        cage.read_bytes(offset + DATA, &mut bytes);
        bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    } else {
        let mut bytes = vec![0u8; len];
        cage.read_bytes(offset + DATA, &mut bytes);
        bytes.into_iter().map(u16::from).collect()
    }
}

/// The string as Rust text (lone surrogates become U+FFFD).
pub fn to_rust(nogc: NoGc<'_>, s: Gc<'_, JsString>) -> String {
    String::from_utf16_lossy(&units(nogc, s))
}

/// Whether `s` is an interned atom.
pub fn is_atom(nogc: NoGc<'_>, s: Gc<'_, JsString>) -> bool {
    header(nogc, s) & ATOM != 0
}

/// The atom with these code units, interning it if needed.
pub fn atom_units(
    s: &mut Scope<'_>,
    t: &mut Tables,
    units: &[u16],
) -> Result<Unbound<JsString>, AllocError> {
    if let Some(g) = t.atoms.get(units) {
        return Ok(g.get(s.nogc()).unbind());
    }
    let a = alloc_with_flags(s, units, ATOM)?;
    let nogc = s.nogc();
    let g = a.bind(nogc);
    t.atoms.insert(units.to_vec(), nogc.global(g));
    Ok(g.unbind())
}

/// The atom for Rust text, interning it if needed.
pub fn atom(
    s: &mut Scope<'_>,
    t: &mut Tables,
    text: &str,
) -> Result<Unbound<JsString>, AllocError> {
    let units: Vec<u16> = text.encode_utf16().collect();
    atom_units(s, t, &units)
}
