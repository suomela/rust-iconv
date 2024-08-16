pub mod ffi {
    #[cfg(not(target_os = "linux"))]
    #[link(name = "iconv")]
    extern "C" {}

    // iconv is part of linux glibc
    #[cfg(target_os = "linux")]
    extern "C" {}

    use libc::{c_char, c_int, c_void, size_t};

    #[allow(non_camel_case_types)]
    pub type iconv_t = *mut c_void;

    extern "C" {
        pub fn iconv_open(__tocode: *const c_char, __fromcode: *const c_char) -> iconv_t;
        pub fn iconv(
            __cd: iconv_t,
            __inbuf: *mut *const u8,
            __inbytesleft: *mut size_t,
            __outbuf: *mut *mut u8,
            __outbytesleft: *mut size_t,
        ) -> size_t;
        pub fn iconv_close(__cd: iconv_t) -> c_int;
    }
}

use libc::size_t;
use std::io::{BufRead, Read, Write};

use dyn_buf::VecBuf;

const MIN_WRITE: usize = 4096;

/// The representation of a iconv converter
pub struct Iconv {
    cd: ffi::iconv_t,
}

#[derive(Debug)]
pub enum IconvError {
    ConversionNotSupport,
    OsError(i32),
    IncompleteInput,
    InvalidInput,
    NotSufficientOutput,
}

impl IconvError {
    pub fn into_io_error(self) -> std::io::Error {
        match self {
            IconvError::OsError(e) => std::io::Error::from_raw_os_error(e),
            IconvError::ConversionNotSupport => {
                std::io::Error::new(std::io::ErrorKind::Unsupported, self)
            }
            IconvError::NotSufficientOutput => {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, self)
            }
            IconvError::InvalidInput => std::io::Error::new(std::io::ErrorKind::InvalidData, self),
            IconvError::IncompleteInput => {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, self)
            }
        }
    }
}

impl std::fmt::Display for IconvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IconvError::OsError(e) => write!(f, "{}", std::io::Error::from_raw_os_error(*e)),
            IconvError::ConversionNotSupport => {
                write!(f, "The conversion is not supported by the implementation")
            }
            IconvError::NotSufficientOutput => {
                write!(f, "There is not sufficient room in the output")
            }
            IconvError::InvalidInput => write!(
                f,
                "An invalid multibyte sequence has been encountered in the input"
            ),
            IconvError::IncompleteInput => write!(
                f,
                "An incomplete multibyte sequence has been encountered in the input"
            ),
        }
    }
}

impl std::error::Error for IconvError {}

/// convert `input` from `from_encoding` to `to_encoding`
pub fn iconv(input: &[u8], from_encoding: &str, to_encoding: &str) -> Result<Vec<u8>, IconvError> {
    let mut c = Iconv::new(from_encoding, to_encoding)?;
    let mut read = 0;
    let mut output = VecBuf::new(MIN_WRITE);
    loop {
        match c.convert(&input[read..], output.prepare_at_least(0)) {
            Ok((r, w, _)) => {
                output.commit(w);
                if read >= input.len() {
                    return Ok(output.into_vec());
                }
                read += r;
            }
            Err((r, w, IconvError::NotSufficientOutput)) => {
                output.commit(w);
                read += r;
                output.grow(0);
            }
            Err((_, _, e)) => return Err(e),
        }
    }
}

/// convert `input` from UTF-8 to `encoding`
pub fn encode(input: &str, encoding: &str) -> Result<Vec<u8>, IconvError> {
    iconv(input.as_bytes(), "UTF-8", encoding)
}

/// convert `input` from `encoding` to UTF-8
pub fn decode(input: &[u8], encoding: &str) -> Result<String, IconvError> {
    iconv(input, encoding, "UTF-8").map(|v| unsafe { String::from_utf8_unchecked(v) })
}

pub fn copy<R: Read, W: Write>(
    input: R,
    mut output: W,
    from_encoding: &str,
    to_encoding: &str,
) -> std::io::Result<usize> {
    let mut cr =
        IconvReader::new(input, from_encoding, to_encoding).map_err(|e| e.into_io_error())?;
    let mut w = 0;
    loop {
        let v = cr.fill_buf()?;
        output.write_all(v)?;
        let n = v.len();
        cr.consume(n);
        w += n;
        if n == 0 {
            return Ok(w);
        }
    }
}

impl Iconv {
    /// Creates a new Converter from `from_encoding` to `to_encoding`.
    pub fn new(from_encoding: &str, to_encoding: &str) -> Result<Iconv, IconvError> {
        use std::ffi::CString;
        let from_code = CString::new(from_encoding).unwrap();
        let to_code = CString::new(to_encoding).unwrap();

        let handle = unsafe { ffi::iconv_open(to_code.as_ptr(), from_code.as_ptr()) };
        if handle as isize == -1 {
            let e = std::io::Error::last_os_error().raw_os_error().unwrap();
            return Err(if e == libc::EINVAL {
                IconvError::ConversionNotSupport
            } else {
                IconvError::OsError(e)
            });
        }
        Ok(Iconv { cd: handle })
    }

    /// reset to the initial state
    pub fn reset(&mut self) {
        use std::ptr::null_mut;
        unsafe { ffi::iconv(self.cd, null_mut(), null_mut(), null_mut(), null_mut()) };
    }

    /// Convert from input into output.
    /// Returns Ok((bytes_read, bytes_written, number_of_chars_converted)).
    ///      or Err((bytes_read, bytes_written, IconvError))
    pub fn convert(
        &mut self,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(usize, usize, usize), (usize, usize, IconvError)> {
        let mut input_left = input.len() as size_t;
        let mut output_left = output.len() as size_t;
        let input_left_ptr = &mut input_left;
        let output_left_ptr = &mut output_left;

        let mut input_ptr = input.as_ptr();
        let mut output_ptr = output.as_mut_ptr();
        let input_ptr_ptr: *mut *const u8 = &mut input_ptr;
        let output_ptr_ptr: *mut *mut u8 = &mut output_ptr;

        let chars = unsafe {
            ffi::iconv(
                self.cd,
                if input.is_empty() {
                    std::ptr::null_mut()
                } else {
                    input_ptr_ptr
                },
                input_left_ptr,
                output_ptr_ptr,
                output_left_ptr,
            )
        };
        let bytes_read = input.len() - input_left as usize;
        let bytes_written = output.len() - output_left as usize;

        if chars as isize != -1 {
            Ok((bytes_read, bytes_written, chars as usize))
        } else {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap();
            Err((
                bytes_read,
                bytes_written,
                match errno {
                    libc::E2BIG => IconvError::NotSufficientOutput,
                    libc::EINVAL => IconvError::IncompleteInput,
                    libc::EILSEQ => IconvError::InvalidInput,
                    _ => IconvError::OsError(errno),
                },
            ))
        }
    }
}

impl Drop for Iconv {
    fn drop(&mut self) {
        unsafe { ffi::iconv_close(self.cd) };
    }
}

pub struct IconvReader<R: Read> {
    iconv: Iconv,
    reader: R,
    input: VecBuf,
    output: VecBuf,
}

impl<R: Read> IconvReader<R> {
    pub fn new(reader: R, from_encoding: &str, to_encoding: &str) -> Result<Self, IconvError> {
        let iconv = Iconv::new(from_encoding, to_encoding)?;
        Ok(Self {
            iconv,
            reader,
            input: VecBuf::new(MIN_WRITE),
            output: VecBuf::new(MIN_WRITE),
        })
    }

    pub fn into_inner(self) -> R {
        self.reader
    }
}

pub struct IconvWriter<W: Write> {
    iconv: Iconv,
    writer: W,
    input: VecBuf,
    output: VecBuf,
}

impl<W: Write> IconvWriter<W> {
    pub fn new(writer: W, from_encoding: &str, to_encoding: &str) -> Result<Self, IconvError> {
        let iconv = Iconv::new(from_encoding, to_encoding)?;
        Ok(Self {
            iconv,
            writer,
            input: VecBuf::new(MIN_WRITE),
            output: VecBuf::new(MIN_WRITE),
        })
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

impl<R: Read> Read for IconvReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut wrote = 0;
        loop {
            let n = self.reader.read(self.input.prepare_at_least(0))?;
            self.input.commit(n);

            match self.iconv.convert(self.input.data(), &mut buf[wrote..]) {
                Ok((r, w, _)) => {
                    self.input.consume(r);
                    wrote += w;
                    return Ok(wrote);
                }
                Err((r, w, e @ IconvError::NotSufficientOutput)) => {
                    self.input.consume(r);
                    wrote += w;
                    return if wrote > 0 {
                        Ok(wrote)
                    } else {
                        Err(e.into_io_error())
                    };
                }
                Err((r, w, e @ IconvError::IncompleteInput)) => {
                    self.input.consume(r);
                    wrote += w;
                    if n == 0 {
                        return if wrote > 0 {
                            Ok(wrote)
                        } else {
                            Err(e.into_io_error())
                        };
                    }
                }
                Err((_, _, e)) => return Err(e.into_io_error()),
            }
        }
    }
}

impl<R: Read> BufRead for IconvReader<R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.output.is_empty() {
            let mut o = std::mem::take(&mut self.output);
            let n = self.read(o.prepare_at_least(0))?;
            o.commit(n);
            let _ = std::mem::replace(&mut self.output, o);
        }
        Ok(self.output.data())
    }

    fn consume(&mut self, amt: usize) {
        self.output.consume(amt)
    }
}

impl<W: Write> Write for IconvWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.input.is_empty() {
            match self.iconv.convert(buf, self.output.prepare_at_least(0)) {
                Ok((r, w, _)) | Err((r, w, IconvError::IncompleteInput)) => {
                    self.output.commit(w);

                    let n = self.writer.write(self.output.data())?;
                    self.output.consume(n);

                    Ok(r)
                }
                Err((_, _, e)) => Err(e.into_io_error()),
            }
        } else {
            self.input.write_all(buf);

            match self
                .iconv
                .convert(self.input.data(), self.output.prepare_at_least(0))
            {
                Ok((r, w, _)) | Err((r, w, IconvError::IncompleteInput)) => {
                    self.input.consume(r);
                    self.output.commit(w);

                    let n = self.writer.write(self.output.data())?;
                    self.output.consume(n);

                    Ok(buf.len())
                }
                Err((_, _, e)) => Err(e.into_io_error()),
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.write(&[])?;

        if !self.input.is_empty() {
            return Err(IconvError::IncompleteInput.into_io_error());
        }
        let b = self.output.data();
        self.writer.write_all(b)?;
        let n = b.len();
        self.output.consume(n);
        self.writer.flush()
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let w = self.write(buf)?;
        if w < buf.len() {
            self.input.write_all(&buf[w..]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::{
        io,
        io::{BufReader, Read},
        iter,
    };

    use super::*;

    #[test]
    fn test_reader() {
        let a = "噗哈";
        let a_gbk = [224u8, 219, 185, 254];
        let mut input = String::new();
        let mut gbk: Vec<u8> = Vec::new();
        for i in 0..1024 {
            let i = i.to_string();
            input.push_str(&i);
            input.push_str(a);
            gbk.extend(i.as_bytes());
            gbk.extend(a_gbk);
        }

        let r = BufReader::new(input.as_bytes());
        let mut cr = IconvReader::new(r, "UTF-8", "GBK").unwrap();

        let mut nread = 0;
        let mut k = 0;
        loop {
            k = (k + 1) % 10 + 1;
            let mut buf = [0u8; 11];
            let res = cr.read(&mut buf[..k]);
            println!("{:?}", res);
            match res {
                Ok(n) if n == 0 => {
                    assert_eq!(nread, gbk.len());
                    return;
                }
                Ok(n) => {
                    assert_eq!(&buf[..n], &gbk[nread..nread + n]);
                    nread += n;
                }
                Err(ref e) if e.kind() == io::ErrorKind::InvalidInput => {
                    return;
                }
                _ => {
                    unreachable!();
                }
            }
        }
    }

    #[test]
    fn test_buf_reader() {
        let a = "噗哈";
        let a_gbk = [224u8, 219, 185, 254];
        let mut input = String::new();
        let mut gbk: Vec<u8> = Vec::new();
        for i in 0..102400 {
            let i = i.to_string();
            input.push_str(&i);
            input.push_str(a);
            gbk.extend(i.as_bytes());
            gbk.extend(a_gbk);
        }

        let r = BufReader::new(input.as_bytes());
        let mut cr = IconvReader::new(r, "UTF-8", "GBK").unwrap();

        let mut nread = 0;
        loop {
            let res = cr.fill_buf().unwrap();
            let n = res.len();
            println!("{} {}", nread, n);
            if res.is_empty() {
                assert_eq!(nread, gbk.len());
                break;
            }

            assert_eq!(res, &gbk[nread..nread + n]);
            nread += n;

            cr.consume(n);
        }
    }

    #[test]
    fn test_copy() {
        let a = "噗哈";
        let a_gbk = [224u8, 219, 185, 254];
        let mut input = String::new();
        let mut gbk: Vec<u8> = Vec::new();
        for i in 0..102400 {
            let i = i.to_string();
            input.push_str(&i);
            input.push_str(a);
            gbk.extend(i.as_bytes());
            gbk.extend(a_gbk);
        }

        let r = BufReader::new(input.as_bytes());
        let mut output = vec![];
        let c = copy(r, std::io::BufWriter::new(&mut output), "UTF-8", "GBK").unwrap();
        assert_eq!(c, output.len());
        assert_eq!(output, gbk);
    }

    #[test]
    fn test_writer() {
        let a = "噗哈";
        let a_gbk = [224u8, 219, 185, 254];
        let mut writer = IconvWriter::new(vec![], "UTF-8", "GBK").unwrap();
        let mut gbk: Vec<u8> = Vec::new();
        for i in 0..102400 {
            let i = i.to_string();
            writer.write_all(i.as_bytes()).unwrap();
            writer.write_all(a.as_bytes()).unwrap();
            gbk.extend(i.as_bytes());
            gbk.extend(a_gbk);
        }

        assert_eq!(&writer.into_inner(), &gbk);
    }

    #[test]
    fn test_encoder_normal() {
        assert!(encode("", "LATIN1").unwrap().is_empty());

        let a = "哈哈";
        assert_eq!(encode(a, "GBK").unwrap(), vec!(0xb9, 0xfe, 0xb9, 0xfe));

        let b = iter::repeat(a).take(1024).collect::<Vec<&str>>().join("");

        for ch in encode(&b, "GBK").unwrap().chunks(4) {
            assert_eq!(ch, &vec![0xb9, 0xfe, 0xb9, 0xfe][..]);
        }

        let c = vec![0xe5, 0x93, 0x88, 0xe5, 0x93, 0x88]; // utf8 bytes
        assert_eq!(
            iconv(&c, "UTF-8", "GBK").unwrap(),
            vec!(0xb9, 0xfe, 0xb9, 0xfe)
        );
    }

    #[test]
    fn test_encoder_fail_creating_converter() {
        assert!(decode("".as_bytes(), "NOT_EXISTS").is_err());
    }

    #[test]
    fn test_encoder_ilseq() {
        let a = vec![0xff, 0xff, 0xff];
        assert!(matches!(
            decode(&a, "GBK").unwrap_err(),
            IconvError::InvalidInput
        ));
    }

    #[test]
    fn test_encoder_invalid() {
        let a = vec![0xe5, 0x93, 0x88, 0xe5, 0x88]; // incomplete utf8 bytes
        assert!(matches!(
            decode(&a, "GBK").unwrap_err(),
            IconvError::IncompleteInput
        ));
    }

    #[test]
    fn test_decoder_normal() {
        let buf = Vec::new();
        let b = &buf[..];
        assert_eq!(decode(b, "CP936").unwrap(), "".to_string());

        let a = vec![0xb9, 0xfe, 0xb9, 0xfe];
        assert_eq!(decode(&a, "GBK").unwrap(), "哈哈".to_string());
    }

    #[test]
    fn test_decoder_fail_creating_converter() {
        let buf = Vec::new();
        let b = &buf[..];
        assert!(matches!(
            decode(b, "NOT_EXSITS").unwrap_err(),
            IconvError::ConversionNotSupport
        ));
    }

    #[test]
    fn test_decoder_ilseq() {
        let a = vec![0xff, 0xff, 0xff];
        assert!(matches!(
            decode(&a, "GBK").unwrap_err(),
            IconvError::InvalidInput
        ));
    }

    #[test]
    fn test_decoder_invalid() {
        let a = vec![0xb9, 0xfe, 0xb9]; // incomplete gbk bytes
        assert!(matches!(
            decode(&a, "GBK").unwrap_err(),
            IconvError::IncompleteInput
        ));
    }

    #[test]
    fn test_caocao_joke() {
        let a = "曹操";
        let b = "变巨";
        assert_eq!(encode(a, "BIG5").unwrap(), encode(b, "GBK").unwrap());
    }
}
