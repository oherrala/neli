//! This is the module that contains the error types used in `neli`
//!
//! There are four main types:
//! * `Nlmsgerr` - an error returned from netlink at the protocol level
//! * `NlError` - typically socket errors
//! * `DeError` - error while deserializing
//! * `SerError` - error while serializing
//!
//! # Design decisions
//! All errors implement `std::error::Error` in an attempt to allow
//! them to be used in conjunction with `Result` for easier error
//! management even at the protocol error level.

use std::{
    error::Error,
    fmt::{self, Debug, Display},
    io, str, string,
};

use bytes::{Bytes, BytesMut};

use crate::{
    consts::NlType,
    nl::{NlEmpty, Nlmsghdr},
    Nl,
};

/// Struct representing netlink packets containing errors
#[derive(Debug)]
pub struct Nlmsgerr<T> {
    /// Error code
    pub error: libc::c_int,
    /// Packet header for request that failed
    pub nlmsg: Nlmsghdr<T, NlEmpty>,
}

impl<T> Display for Nlmsgerr<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", io::Error::from_raw_os_error(self.error))
    }
}

impl<T> Error for Nlmsgerr<T> where T: Debug {}

impl<T> Nl for Nlmsgerr<T>
where
    T: NlType,
{
    fn serialize(&self, mem: BytesMut) -> Result<BytesMut, SerError> {
        Ok(serialize! {
            PAD self;
            mem;
            self.error;
            self.nlmsg
        })
    }

    fn deserialize(mem: Bytes) -> Result<Self, DeError> {
        Ok(deserialize! {
            STRIP Self;
            mem;
            Nlmsgerr {
                error: libc::c_int,
                nlmsg: Nlmsghdr<T, NlEmpty> => mem.len().checked_sub(libc::c_int::type_size()
                    .expect("Integers have static sizes"))
                    .ok_or_else(|| DeError::UnexpectedEOB)?
            } => mem.len()
        })
    }

    fn size(&self) -> usize {
        self.error.size() + self.nlmsg.size()
    }

    fn type_size() -> Option<usize> {
        Nlmsghdr::<T, NlEmpty>::type_size()
            .and_then(|nhdr_sz| libc::c_int::type_size().map(|cint| cint + nhdr_sz))
    }
}

macro_rules! err_from {
    ($err:ident, $($from_err:path { $from_impl:expr }),+) => {
        $(
            impl From<$from_err> for $err {
                fn from(e: $from_err) -> Self {
                    $from_impl(e)
                }
            }
        )*
    };
}

/// Netlink protocol error
#[derive(Debug)]
pub enum NlError {
    /// Type indicating a message from a converted error
    Msg(String),
    /// An error packet sent back by netlink
    Nlmsgerr(Box<Nlmsgerr<u16>>),
    /// A wrapped error from lower in the call stack
    Wrapped(WrappedError),
    /// No ack was received when `NlmF::Ack` was specified in the request
    NoAck,
    /// The sequence number for the response did not match the request
    BadSeq,
    /// Incorrect PID socket identifier in received message
    BadPid,
    /// `SerError` wrapper
    Ser(SerError),
    /// `DeError` wrapper
    De(DeError),
}

err_from!(
    NlError,
    Nlmsgerr<u16> { |e| NlError::Nlmsgerr(Box::new(e)) },
    WrappedError { NlError::Wrapped },
    std::io::Error { |e| NlError::Wrapped(WrappedError::from(e)) },
    std::str::Utf8Error { |e| NlError::Wrapped(WrappedError::from(e)) },
    std::string::FromUtf8Error { |e| NlError::Wrapped(WrappedError::from(e)) },
    std::ffi::FromBytesWithNulError { |e| NlError::Wrapped(WrappedError::from(e)) },
    SerError { NlError::Ser },
    DeError { NlError::De }
);

impl NlError {
    /// Create new error from a data type implementing `Display`
    pub fn new<D>(s: D) -> Self
    where
        D: Display,
    {
        NlError::Msg(s.to_string())
    }
}

/// Netlink protocol error
impl Display for NlError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            NlError::Msg(ref msg) => write!(f, "{}", msg),
            NlError::Nlmsgerr(ref err) => {
                write!(f, "Error response received from netlink: {}", err)
            }
            NlError::NoAck => write!(f, "No ack received"),
            NlError::BadSeq => write!(f, "Sequence number does not match the request"),
            NlError::BadPid => write!(f, "PID does not match the socket"),
            NlError::Wrapped(ref e) => write!(f, "Netlink failure due to error: {}", e),
            NlError::De(ref e) => {
                write!(f, "Netlink failure due to deserialization failure: {}", e)
            }
            NlError::Ser(ref e) => write!(f, "Netlink failure due to serialization failure: {}", e),
        }
    }
}

impl Error for NlError {}

/// Serialization error
#[derive(Debug)]
pub enum SerError {
    /// Abitrary error message
    Msg(String, BytesMut),
    /// A wrapped error from lower in the call stack
    Wrapped(WrappedError, BytesMut),
    /// The end of the buffer was reached before serialization finished
    UnexpectedEOB(BytesMut),
    /// Serialization did not fill the buffer
    BufferNotFilled(BytesMut),
}

impl SerError {
    /// Create a new error with the given message as description
    pub fn new<D>(msg: D, bytes: BytesMut) -> Self
    where
        D: Display,
    {
        SerError::Msg(msg.to_string(), bytes)
    }

    /// Reconstruct `BytesMut` at current level to bubble error up
    pub fn reconstruct(self, start: Option<BytesMut>, end: Option<BytesMut>) -> Self {
        match (start, end) {
            (Some(mut s), Some(e)) => match self {
                SerError::BufferNotFilled(b) => {
                    s.unsplit(b);
                    s.unsplit(e);
                    SerError::BufferNotFilled(s)
                }
                SerError::UnexpectedEOB(b) => {
                    s.unsplit(b);
                    s.unsplit(e);
                    SerError::UnexpectedEOB(s)
                }
                SerError::Msg(m, b) => {
                    s.unsplit(b);
                    s.unsplit(e);
                    SerError::Msg(m, s)
                }
                SerError::Wrapped(err, b) => {
                    s.unsplit(b);
                    s.unsplit(e);
                    SerError::Wrapped(err, s)
                }
            },
            (Some(mut s), _) => match self {
                SerError::BufferNotFilled(b) => {
                    s.unsplit(b);
                    SerError::BufferNotFilled(s)
                }
                SerError::UnexpectedEOB(b) => {
                    s.unsplit(b);
                    SerError::UnexpectedEOB(s)
                }
                SerError::Msg(m, b) => {
                    s.unsplit(b);
                    SerError::Msg(m, s)
                }
                SerError::Wrapped(err, b) => {
                    s.unsplit(b);
                    SerError::Wrapped(err, s)
                }
            },
            (_, Some(e)) => match self {
                SerError::BufferNotFilled(mut b) => {
                    b.unsplit(e);
                    SerError::BufferNotFilled(b)
                }
                SerError::UnexpectedEOB(mut b) => {
                    b.unsplit(e);
                    SerError::UnexpectedEOB(b)
                }
                SerError::Msg(m, mut b) => {
                    b.unsplit(e);
                    SerError::Msg(m, b)
                }
                SerError::Wrapped(err, mut b) => {
                    b.unsplit(e);
                    SerError::Wrapped(err, b)
                }
            },
            (_, _) => self,
        }
    }
}

impl Display for SerError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            SerError::Msg(ref s, _) => write!(f, "{}", s),
            SerError::Wrapped(ref e, _) => write!(f, "Error while serializing: {}", e),
            SerError::UnexpectedEOB(_) => write!(
                f,
                "The buffer was too small for the requested serialization operation",
            ),
            SerError::BufferNotFilled(_) => write!(
                f,
                "The number of bytes written to the buffer did not fill the \
                 given space",
            ),
        }
    }
}

impl Error for SerError {}

/// Deserialization error
#[derive(Debug)]
pub enum DeError {
    /// Abitrary error message
    Msg(String),
    /// A wrapped error from lower in the call stack
    Wrapped(WrappedError),
    /// The end of the buffer was reached before deserialization finished
    UnexpectedEOB,
    /// Deserialization did not fill the buffer
    BufferNotParsed,
    /// A null byte was found before the end of the serialized `String`
    NullError,
    /// A null byte was not found at the end of the serialized `String`
    NoNullError,
}

impl DeError {
    /// Create new error from `&str`
    pub fn new<D>(s: D) -> Self
    where
        D: Display,
    {
        DeError::Msg(s.to_string())
    }
}

impl Display for DeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            DeError::Msg(ref s) => write!(f, "{}", s),
            DeError::UnexpectedEOB => write!(
                f,
                "The buffer was not large enough to complete the deserialize \
                 operation",
            ),
            DeError::BufferNotParsed => write!(f, "Unparsed data left in buffer"),
            DeError::NullError => write!(f, "A null was found before the end of the buffer"),
            DeError::NoNullError => write!(f, "No terminating null byte was found in the buffer"),
            DeError::Wrapped(ref e) => write!(f, "Error while deserializing: {}", e),
        }
    }
}

impl Error for DeError {}

/// An error to wrap all system level errors in a single, higher level
/// error.
#[derive(Debug)]
pub enum WrappedError {
    /// Wrapper for `std::io::Error`
    IOError(io::Error),
    /// Wrapper for `std::str::Utf8Error`
    StrUtf8Error(str::Utf8Error),
    /// Wrapper for `std::string::FromUtf8Error`
    StringUtf8Error(string::FromUtf8Error),
    /// Wrapper for `std::ffi::FromBytesWithNulError`
    FFINullError(std::ffi::FromBytesWithNulError),
}

impl Display for WrappedError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            WrappedError::IOError(ref e) => write!(f, "Wrapped IO error: {}", e),
            WrappedError::StrUtf8Error(ref e) => write!(f, "Wrapped &str error: {}", e),
            WrappedError::StringUtf8Error(ref e) => write!(f, "Wrapped String error: {}", e),
            WrappedError::FFINullError(ref e) => write!(f, "Wrapped null error: {}", e),
        }
    }
}

impl Error for WrappedError {}

macro_rules! wrapped_err_from {
    ($($var:ident => $from_err_name:path),*) => {
        $(
            impl From<$from_err_name> for WrappedError {
                fn from(v: $from_err_name) -> Self {
                    WrappedError::$var(v)
                }
            }
        )*
    }
}

wrapped_err_from!(
    IOError => std::io::Error,
    StrUtf8Error => std::str::Utf8Error,
    StringUtf8Error => std::string::FromUtf8Error,
    FFINullError => std::ffi::FromBytesWithNulError
);
