//! A strict DER reader: definite lengths only, minimal length encoding, no trailing garbage.

use crate::Error;

/// Universal and context tags used by X.509.
pub mod tag {
    /// BOOLEAN.
    pub const BOOLEAN: u8 = 0x01;
    /// INTEGER.
    pub const INTEGER: u8 = 0x02;
    /// BIT STRING.
    pub const BIT_STRING: u8 = 0x03;
    /// OCTET STRING.
    pub const OCTET_STRING: u8 = 0x04;
    /// NULL.
    pub const NULL: u8 = 0x05;
    /// OBJECT IDENTIFIER.
    pub const OID: u8 = 0x06;
    /// UTCTime.
    pub const UTC_TIME: u8 = 0x17;
    /// GeneralizedTime.
    pub const GENERALIZED_TIME: u8 = 0x18;
    /// SEQUENCE.
    pub const SEQUENCE: u8 = 0x30;
    /// SET.
    pub const SET: u8 = 0x31;
    /// Context-specific constructed `[n]`.
    #[must_use]
    pub const fn context(n: u8) -> u8 {
        0xA0 | n
    }
}

/// One parsed element: its tag, its content, and its full encoding (header included).
#[derive(Clone, Copy, Debug)]
pub struct Element<'a> {
    /// Identifier octet.
    pub tag: u8,
    /// Content octets.
    pub content: &'a [u8],
    /// Header plus content, as it appeared in the input.
    pub raw: &'a [u8],
}

/// A cursor over a run of DER elements.
#[derive(Clone, Copy, Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    /// Read from `data`.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Whether everything has been consumed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The next tag without consuming it.
    #[must_use]
    pub fn peek_tag(&self) -> Option<u8> {
        self.data.first().copied()
    }

    /// Consume the next element.
    ///
    /// # Errors
    /// [`Error::Der`] for truncated, indefinite, over-long or non-minimal encodings.
    pub fn read(&mut self) -> Result<Element<'a>, Error> {
        let d = self.data;
        let tag = *d.first().ok_or(Error::Der("unexpected end"))?;
        if tag & 0x1F == 0x1F {
            return Err(Error::Der("high tag numbers are not used"));
        }
        let l0 = *d.get(1).ok_or(Error::Der("truncated length"))?;
        let (len, hdr) = if l0 < 0x80 {
            (usize::from(l0), 2)
        } else {
            let n = usize::from(l0 & 0x7F);
            if n == 0 {
                return Err(Error::Der("indefinite length"));
            }
            if n > 4 {
                return Err(Error::Der("length too large"));
            }
            let bytes = d.get(2..2 + n).ok_or(Error::Der("truncated length"))?;
            let mut len = 0usize;
            for &b in bytes {
                len = len << 8 | usize::from(b);
            }
            if bytes[0] == 0 || len < 0x80 {
                return Err(Error::Der("non-minimal length"));
            }
            (len, 2 + n)
        };
        let end = hdr.checked_add(len).ok_or(Error::Der("length overflow"))?;
        if end > d.len() {
            return Err(Error::Der("content past end"));
        }
        self.data = &d[end..];
        Ok(Element {
            tag,
            content: &d[hdr..end],
            raw: &d[..end],
        })
    }

    /// Consume the next element, which must have `tag`.
    ///
    /// # Errors
    /// [`Error::Der`] on a different tag or bad encoding.
    pub fn expect(&mut self, tag: u8) -> Result<Element<'a>, Error> {
        let e = self.read()?;
        if e.tag == tag {
            Ok(e)
        } else {
            Err(Error::Der("unexpected tag"))
        }
    }

    /// Consume the next element only when it has `tag`.
    ///
    /// # Errors
    /// [`Error::Der`] on bad encoding.
    pub fn optional(&mut self, tag: u8) -> Result<Option<Element<'a>>, Error> {
        if self.peek_tag() == Some(tag) {
            self.read().map(Some)
        } else {
            Ok(None)
        }
    }

    /// Fail unless everything was consumed.
    ///
    /// # Errors
    /// [`Error::Der`] when bytes remain.
    pub fn finish(&self) -> Result<(), Error> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(Error::Der("trailing data"))
        }
    }
}

/// Parse `data` as exactly one element with `tag` and return a reader over its content.
///
/// # Errors
/// [`Error::Der`] on a mismatch or trailing bytes.
pub fn single(data: &[u8], tag: u8) -> Result<Reader<'_>, Error> {
    let mut r = Reader::new(data);
    let e = r.expect(tag)?;
    r.finish()?;
    Ok(Reader::new(e.content))
}

/// The unsigned value bytes of an INTEGER (leading zero removed). Rejects negatives and
/// non-minimal encodings.
///
/// # Errors
/// [`Error::Der`] for a negative or padded integer.
pub fn unsigned_integer(content: &[u8]) -> Result<&[u8], Error> {
    match content {
        [] => Err(Error::Der("empty integer")),
        [first, ..] if first & 0x80 != 0 => Err(Error::Der("negative integer")),
        [0, second, ..] if second & 0x80 == 0 => Err(Error::Der("non-minimal integer")),
        [0, rest @ ..] if !rest.is_empty() => Ok(rest),
        _ => Ok(content),
    }
}

/// The bytes of a BIT STRING with no unused bits.
///
/// # Errors
/// [`Error::Der`] when unused bits are declared.
pub fn whole_bit_string(content: &[u8]) -> Result<&[u8], Error> {
    match content.split_first() {
        Some((0, rest)) => Ok(rest),
        _ => Err(Error::Der("bit string has unused bits")),
    }
}

/// Decode an OBJECT IDENTIFIER's content to dotted form, for matching and messages.
///
/// # Errors
/// [`Error::Der`] for an empty or truncated identifier.
pub fn oid_string(content: &[u8]) -> Result<String, Error> {
    let (first, rest) = content.split_first().ok_or(Error::Der("empty oid"))?;
    let mut out = format!("{}.{}", first / 40, first % 40);
    let mut value = 0u64;
    let mut open = false;
    for &b in rest {
        if value > u64::MAX >> 8 {
            return Err(Error::Der("oid arc too large"));
        }
        value = value << 7 | u64::from(b & 0x7F);
        open = b & 0x80 != 0;
        if !open {
            out.push('.');
            out.push_str(&value.to_string());
            value = 0;
        }
    }
    if open {
        return Err(Error::Der("truncated oid"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_elements() {
        let der = [0x30, 0x06, 0x02, 0x01, 0x05, 0x04, 0x01, 0xAA];
        let mut r = single(&der, tag::SEQUENCE).unwrap();
        let i = r.expect(tag::INTEGER).unwrap();
        assert_eq!(i.content, [5]);
        assert_eq!(i.raw, [0x02, 0x01, 0x05]);
        assert_eq!(r.expect(tag::OCTET_STRING).unwrap().content, [0xAA]);
        assert!(r.finish().is_ok());
    }

    #[test]
    fn rejects_malformed_lengths() {
        for bad in [
            &[0x30][..],
            &[0x30, 0x80][..],             // indefinite
            &[0x30, 0x81, 0x05][..],       // non-minimal long form
            &[0x30, 0x82, 0x00, 0x80][..], // leading zero in length
            &[0x30, 0x05, 0x00][..],       // content past end
            &[0x1F, 0x01, 0x00][..],       // high tag
        ] {
            assert!(Reader::new(bad).read().is_err(), "{bad:?}");
        }
        assert!(single(&[0x05, 0x00, 0x00], tag::NULL).is_err()); // trailing
    }

    #[test]
    fn long_form_lengths() {
        let mut der = vec![0x04, 0x82, 0x01, 0x00];
        der.extend(std::iter::repeat_n(7, 256));
        let e = Reader::new(&der).read().unwrap();
        assert_eq!(e.content.len(), 256);
    }

    #[test]
    fn integers() {
        assert_eq!(unsigned_integer(&[0x00, 0x80]).unwrap(), [0x80]);
        assert_eq!(unsigned_integer(&[0x7F]).unwrap(), [0x7F]);
        assert_eq!(unsigned_integer(&[0x00]).unwrap(), [0x00]);
        assert!(unsigned_integer(&[0x80]).is_err());
        assert!(unsigned_integer(&[0x00, 0x01]).is_err());
        assert!(unsigned_integer(&[]).is_err());
    }

    #[test]
    fn oids() {
        // 1.2.840.113549.1.1.11 (sha256WithRSAEncryption)
        let c = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B];
        assert_eq!(oid_string(&c).unwrap(), "1.2.840.113549.1.1.11");
        assert!(oid_string(&[0x2A, 0x86]).is_err());
        assert!(oid_string(&[]).is_err());
    }

    #[test]
    fn bit_strings() {
        assert_eq!(whole_bit_string(&[0, 1, 2]).unwrap(), [1, 2]);
        assert!(whole_bit_string(&[3, 1]).is_err());
    }
}
