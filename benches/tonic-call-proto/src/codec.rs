//! A `tonic::codec::Codec` driven by a `prost_reflect::MessageDescriptor`
//! instead of a compile-time prost type — the shape abada's future `service`
//! module needs, since there is no per-RPC generated code yet: the request
//! reaching a handler is already a `prost_reflect::DynamicMessage`
//! (`abada::request::RequestBinding::decode`'s output), not a typed message.

use bytes::Buf;
use prost::Message;
use prost_reflect::{DynamicMessage, MessageDescriptor, ReflectMessage};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

#[derive(Clone)]
pub struct DynamicCodec {
    encode_desc: MessageDescriptor,
    decode_desc: MessageDescriptor,
}

impl DynamicCodec {
    pub fn new(encode_desc: MessageDescriptor, decode_desc: MessageDescriptor) -> Self {
        Self {
            encode_desc,
            decode_desc,
        }
    }
}

#[derive(Clone)]
pub struct DynamicEncoder {
    desc: MessageDescriptor,
}

#[derive(Clone)]
pub struct DynamicDecoder {
    desc: MessageDescriptor,
}

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        debug_assert_eq!(
            item.descriptor().full_name(),
            self.desc.full_name(),
            "DynamicCodec: encoded message does not match the descriptor it was built for"
        );
        item.encode(dst)
            .map_err(|e| tonic::Status::internal(format!("encode: {e}")))
    }
}

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        let mut msg = DynamicMessage::new(self.desc.clone());
        let mut buf = src.copy_to_bytes(src.remaining());
        msg.merge(&mut buf)
            .map_err(|e| tonic::Status::invalid_argument(format!("decode: {e}")))?;
        Ok(Some(msg))
    }
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder {
            desc: self.encode_desc.clone(),
        }
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder {
            desc: self.decode_desc.clone(),
        }
    }
}
