//! Protocol Buffer message definitions for the Google Cast V2 protocol.
//!
//! These are hand-written [`prost`](https://docs.rs/prost) definitions that mirror
//! Google's `cast_channel.proto` (proto2). They are kept hand-written (instead of
//! generated with `prost-build`) so this crate builds without requiring `protoc`.
//!
//! Wire format facts (verified against the Cast V2 spec):
//! * Every `CastMessage` is framed with a 4-byte big-endian length prefix over TLS.
//! * JSON payloads ride in `payload_utf8` (payload_type = STRING).
//! * The device-auth handshake rides in `payload_binary` (payload_type = BINARY).

pub mod cast_channel {
    //! Mirrors `cast_channel.proto` (package `cast_channel`).

    /// Version of the Cast protocol.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum ProtocolVersion {
        Castv2_1_0 = 0,
        Castv2_1_1 = 1,
        Castv2_1_2 = 2,
        Castv2_1_3 = 3,
    }

    /// How the payload is carried inside a `CastMessage`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum PayloadType {
        String = 0,
        Binary = 1,
    }

    /// The envelope used for every Cast V2 message over the TLS channel.
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct CastMessage {
        #[prost(enumeration = "ProtocolVersion", required, tag = "1")]
        pub protocol_version: i32,
        #[prost(string, required, tag = "2")]
        pub source_id: ::prost::alloc::string::String,
        #[prost(string, required, tag = "3")]
        pub destination_id: ::prost::alloc::string::String,
        #[prost(string, required, tag = "4")]
        pub namespace: ::prost::alloc::string::String,
        #[prost(enumeration = "PayloadType", required, tag = "5")]
        pub payload_type: i32,
        #[prost(string, optional, tag = "6")]
        pub payload_utf8: ::core::option::Option<::prost::alloc::string::String>,
        #[prost(bytes = "vec", optional, tag = "7")]
        pub payload_binary: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
        /// Whether more chunks follow (Cast V2 1.1+ message chunking).
        #[prost(bool, optional, tag = "8")]
        pub continued: ::core::option::Option<bool>,
        /// Remaining payload length across chunks.
        #[prost(uint32, optional, tag = "9")]
        pub remaining_length: ::core::option::Option<u32>,
    }

    /// Signature algorithm used in the device-auth handshake.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum SignatureAlgorithm {
        Unspecified = 0,
        RsassaPkcs1v15 = 1,
        RsassaPss = 2,
    }

    /// Hash algorithm used in the device-auth handshake.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum HashAlgorithm {
        Sha1 = 0,
        Sha256 = 1,
    }

    /// First half of the device authentication handshake (sent by the sender).
    ///
    /// Field numbers verified against openscreen `openscreen.cast.proto` and
    /// live captures (sender_nonce is field 2 in current SDKs).
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct AuthChallenge {
        #[prost(enumeration = "SignatureAlgorithm", optional, tag = "1")]
        pub signature_algorithm: ::core::option::Option<i32>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub sender_nonce: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
        #[prost(enumeration = "HashAlgorithm", optional, tag = "3")]
        pub hash_algorithm: ::core::option::Option<i32>,
    }

    /// Second half of the device authentication handshake (sent by the receiver).
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct AuthResponse {
        #[prost(bytes = "vec", required, tag = "1")]
        pub signature: ::prost::alloc::vec::Vec<u8>,
        #[prost(bytes = "vec", required, tag = "2")]
        pub client_auth_certificate: ::prost::alloc::vec::Vec<u8>,
        #[prost(bytes = "vec", repeated, tag = "3")]
        pub intermediate_certificate: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
        #[prost(enumeration = "SignatureAlgorithm", optional, tag = "4")]
        pub signature_algorithm: ::core::option::Option<i32>,
        #[prost(bytes = "vec", optional, tag = "5")]
        pub sender_nonce: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
        #[prost(enumeration = "HashAlgorithm", optional, tag = "6")]
        pub hash_algorithm: ::core::option::Option<i32>,
        #[prost(bytes = "vec", optional, tag = "7")]
        pub crl: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum ErrorType {
        InternalError = 0,
        NoTls = 1,
        SignatureAlgorithmUnavailable = 2,
    }

    /// Authentication error reported to the sender.
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct AuthError {
        #[prost(enumeration = "ErrorType", required, tag = "1")]
        pub error_type: i32,
    }

    /// Top-level message for the handshake in the `...tp.deviceauth` namespace.
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct DeviceAuthMessage {
        #[prost(message, optional, tag = "1")]
        pub challenge: ::core::option::Option<AuthChallenge>,
        #[prost(message, optional, tag = "2")]
        pub response: ::core::option::Option<AuthResponse>,
        #[prost(message, optional, tag = "3")]
        pub error: ::core::option::Option<AuthError>,
    }
}
