//! Encoding a command to bytes.

use crate::{Checksum, ChecksumKind, Command, Definition, Elem, Error, Param, WireOp};

/// The result of encoding one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    /// The frame before wire transforms.
    pub plain: Vec<u8>,
    /// The bytes to write to the characteristic.
    pub wire: Vec<u8>,
}

fn byte(v: f64) -> u8 {
    // `as` on a clamped, finite f64 is exact here: the value is within 0..=255.
    v.round().clamp(0.0, 255.0) as u8
}

impl Definition {
    /// Encode `command` with `args` (parameter name, value).
    ///
    /// Values are clamped to each parameter's range and rounded. Unknown or missing parameters and non-finite
    /// values are errors, so typos never silently produce a wrong frame.
    ///
    /// # Errors
    /// See [`Error`].
    pub fn encode(&self, command: &str, args: &[(&str, f64)]) -> Result<Encoded, Error> {
        let cmd = self
            .commands
            .get(command)
            .ok_or_else(|| Error::UnknownCommand(command.to_string()))?;
        for (name, value) in args {
            if !uses_param(cmd, name) {
                return Err(Error::UnknownParam {
                    command: command.to_string(),
                    param: (*name).to_string(),
                });
            }
            if !value.is_finite() {
                return Err(Error::BadValue {
                    command: command.to_string(),
                    param: (*name).to_string(),
                });
            }
        }
        let mut plain = Vec::with_capacity(cmd.frame_len());
        for el in &cmd.frame {
            plain.push(match el {
                Elem::Byte(b) => *b,
                Elem::Param(p) => byte(resolve(cmd, command, p, args)?),
                Elem::Scale(s) => {
                    let v = resolve(cmd, command, &s.param, args)?;
                    let t = ((v - s.from[0]) / (s.from[1] - s.from[0])).clamp(0.0, 1.0);
                    byte(s.to[0] + t * (s.to[1] - s.to[0]))
                }
            });
        }
        if let Some(n) = cmd.pad_to {
            plain.resize(n, 0);
        }
        if let Some(Checksum { kind }) = &cmd.checksum {
            let sum = match kind {
                ChecksumKind::Xor => plain.iter().fold(0u8, |a, b| a ^ b),
                ChecksumKind::Sum8 => plain.iter().fold(0u8, |a, b| a.wrapping_add(*b)),
            };
            plain.push(sum);
        }
        let wire = self.apply_wire(&plain)?;
        Ok(Encoded { plain, wire })
    }

    fn apply_wire(&self, plain: &[u8]) -> Result<Vec<u8>, Error> {
        let mut data = plain.to_vec();
        for op in &self.wire {
            match op {
                WireOp::Aes128Ecb { key } => data = aes_ecb(key, &data)?,
            }
        }
        Ok(data)
    }
}

/// A parameter is accepted if it is declared or appears in the frame.
pub(crate) fn uses_param(cmd: &Command, name: &str) -> bool {
    cmd.params.contains_key(name)
        || cmd.frame.iter().any(|e| match e {
            Elem::Param(p) => p == name,
            Elem::Scale(s) => s.param == name,
            Elem::Byte(_) => false,
        })
}

fn resolve(cmd: &Command, command: &str, name: &str, args: &[(&str, f64)]) -> Result<f64, Error> {
    let spec = cmd.params.get(name).cloned().unwrap_or_else(Param::default);
    let given = args.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
    let value = given.or(spec.default).ok_or_else(|| Error::MissingParam {
        command: command.to_string(),
        param: name.to_string(),
    })?;
    Ok(value.clamp(spec.min, spec.max))
}

#[cfg(feature = "aes")]
fn aes_ecb(key_hex: &str, data: &[u8]) -> Result<Vec<u8>, Error> {
    use aes::cipher::{Array, BlockCipherEncrypt, KeyInit};
    let key = crate::from_hex(key_hex)
        .and_then(|k| <[u8; 16]>::try_from(k).ok())
        .ok_or_else(|| Error::Invalid("aes key must be 32 hex characters".into()))?;
    let cipher = aes::Aes128::new(&Array::from(key));
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let block: [u8; 16] = chunk
            .try_into()
            .map_err(|_| Error::Invalid("aes needs whole 16-byte blocks".into()))?;
        let mut b = Array::from(block);
        cipher.encrypt_block(&mut b);
        out.extend_from_slice(&b);
    }
    Ok(out)
}

#[cfg(not(feature = "aes"))]
fn aes_ecb(_key_hex: &str, _data: &[u8]) -> Result<Vec<u8>, Error> {
    Err(Error::Unsupported("this build has no aes feature".into()))
}
