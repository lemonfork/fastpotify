//! Ogg Opus packet decoding for formats Symphonia can demux but not decode.

use opus_pure::{Error as OpusError, MAX_PACKET_SAMPLES, OpusDecoder};
use symphonia::core::codecs::{CODEC_TYPE_OPUS, CodecParameters};

const OPUS_HEAD_MIN_LEN: usize = 19;
const OPUS_SAMPLE_RATE: u32 = 48_000;

/// A decoded Opus packet in the layout advertised by its `OpusHead`.
pub(crate) struct DecodedOpus {
    pub samples: Vec<f32>,
    pub channels: usize,
}

/// Decodes mono and stereo Ogg Opus packets after Symphonia has demuxed them.
///
/// Keeping the container and codec stages separate mirrors the main Symphonia
/// path: metadata never chooses a decoder, the probed codec parameters do.
pub(crate) struct OggOpusDecoder {
    decoder: OpusDecoder,
    channels: usize,
    remaining_pre_skip: usize,
    scratch: Vec<f32>,
}

impl OggOpusDecoder {
    pub const SAMPLE_RATE: u32 = OPUS_SAMPLE_RATE;

    pub fn new(params: &CodecParameters) -> Result<Self, OpusConfigError> {
        if params.codec != CODEC_TYPE_OPUS {
            return Err(OpusConfigError::WrongCodec);
        }
        let head = params
            .extra_data
            .as_deref()
            .ok_or(OpusConfigError::MissingHeader)?;
        if head.len() < OPUS_HEAD_MIN_LEN
            || &head[..8] != b"OpusHead"
            // Ogg Opus starts at version 1. ISO-BMFF's `dOps` box uses 0 and
            // big-endian numeric fields even when a demuxer prepends this
            // magic, so accepting 0 here would misread its pre-skip and gain.
            || !(1..=0x0f).contains(&head[8])
        {
            return Err(OpusConfigError::InvalidHeader);
        }

        let channels = usize::from(head[9]);
        // Mapping family 0 is the canonical mono/stereo layout. Symphonia's
        // Ogg reader can expose surround mappings too, but Fastpotify does not
        // claim them until the whole output/downmix path is covered.
        if !(1..=2).contains(&channels) || head[18] != 0 {
            return Err(OpusConfigError::UnsupportedChannelMapping);
        }
        if params.channels.map(|layout| layout.count()) != Some(channels)
            || params.sample_rate != Some(OPUS_SAMPLE_RATE)
        {
            return Err(OpusConfigError::InvalidHeader);
        }

        let header_pre_skip = u16::from_le_bytes([head[10], head[11]]);
        let gain_q8 = i16::from_le_bytes([head[16], head[17]]);
        let mut decoder = OpusDecoder::new(OPUS_SAMPLE_RATE as i32, channels)
            .map_err(|_| OpusConfigError::UnsupportedChannelMapping)?;
        decoder.gain_q8 = i32::from(gain_q8);

        Ok(Self {
            decoder,
            channels,
            // The Ogg mapping defines this value in OpusHead. Symphonia may
            // derive a larger generic delay from short final pages, so its
            // codec parameter is not authoritative for Opus pre-skip.
            remaining_pre_skip: usize::from(header_pre_skip),
            scratch: vec![0.0; MAX_PACKET_SAMPLES * channels],
        })
    }

    pub fn decode(&mut self, packet: &[u8], trim_end: u32) -> Result<DecodedOpus, OpusError> {
        if packet.is_empty() {
            return Err(OpusError::InvalidPacket("empty Ogg Opus packet"));
        }
        let frames = self
            .decoder
            .decode(packet, MAX_PACKET_SAMPLES, &mut self.scratch)?;
        let skip = self.remaining_pre_skip.min(frames);
        self.remaining_pre_skip -= skip;
        let trailing = usize::try_from(trim_end)
            .unwrap_or(usize::MAX)
            .min(frames.saturating_sub(skip));
        let audible_end = frames - trailing;
        let samples = &self.scratch[skip * self.channels..audible_end * self.channels];
        Ok(DecodedOpus {
            samples: samples.to_vec(),
            channels: self.channels,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpusConfigError {
    WrongCodec,
    MissingHeader,
    InvalidHeader,
    UnsupportedChannelMapping,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use opus_pure::{Application, MAX_PACKET_BYTES, OggOpusWriter, OpusEncoder, OpusHead};
    use symphonia::core::audio::Channels;
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    use super::*;

    fn opus_params(
        channels: u8,
        pre_skip: u16,
        parameter_delay: u32,
        gain_q8: i16,
        mapping_family: u8,
    ) -> CodecParameters {
        let mut head = Vec::from(*b"OpusHead");
        head.extend_from_slice(&[1, channels]);
        head.extend_from_slice(&pre_skip.to_le_bytes());
        head.extend_from_slice(&OPUS_SAMPLE_RATE.to_le_bytes());
        head.extend_from_slice(&gain_q8.to_le_bytes());
        head.push(mapping_family);

        let mut params = CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_OPUS)
            .with_sample_rate(OPUS_SAMPLE_RATE)
            .with_channels(if channels == 1 {
                Channels::FRONT_LEFT
            } else {
                Channels::FRONT_LEFT | Channels::FRONT_RIGHT
            })
            .with_delay(parameter_delay)
            .with_extra_data(head.into_boxed_slice());
        params
    }

    #[test]
    fn stereo_packet_decodes_and_removes_ogg_pre_skip() {
        // Generic Ogg page inspection can report a larger CodecParameters
        // delay for a short final page. OpusHead remains the authority.
        let mut decoder = OggOpusDecoder::new(&opus_params(2, 312, 648, 0, 0)).unwrap();
        // A standards-compliant 20 ms Opus silence packet. It exercises the
        // real packet decoder without checking in audio copied from a server.
        let decoded = decoder.decode(&[0xf8, 0xff, 0xfe], 0).unwrap();

        assert_eq!(decoded.channels, 2);
        assert_eq!(decoded.samples.len(), (960 - 312) * 2);
        assert!(decoded.samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn output_gain_from_opus_head_is_applied() {
        let mut encoder = OpusEncoder::new(OPUS_SAMPLE_RATE as i32, 2, Application::Audio).unwrap();
        let frame_size = 960;
        let mut encoded = vec![0; MAX_PACKET_BYTES];
        let source = (0..frame_size)
            .flat_map(|index| {
                let sample = ((index as f32) * 0.05).sin() * 0.25;
                [sample, sample]
            })
            .collect::<Vec<_>>();
        let packet_len = encoder.encode(&source, frame_size, &mut encoded).unwrap();
        let packet = &encoded[..packet_len];

        let decode_rms = |gain_q8| {
            let mut decoder = OggOpusDecoder::new(&opus_params(2, 312, 312, gain_q8, 0)).unwrap();
            let decoded = decoder.decode(packet, 0).unwrap();
            let energy = decoded
                .samples
                .iter()
                .map(|sample| f64::from(*sample) * f64::from(*sample))
                .sum::<f64>()
                / decoded.samples.len() as f64;
            energy.sqrt()
        };

        let unity = decode_rms(0);
        let quiet = decode_rms(-1536);
        let ratio = quiet / unity;
        assert!(
            (ratio - 0.501).abs() < 0.03,
            "unexpected gain ratio: {ratio}"
        );
    }

    #[test]
    fn unsupported_surround_mapping_is_rejected_before_playback() {
        let params = opus_params(6, 312, 312, 0, 1);
        assert_eq!(
            OggOpusDecoder::new(&params).err(),
            Some(OpusConfigError::UnsupportedChannelMapping)
        );
    }

    #[test]
    fn malformed_opus_head_is_rejected() {
        let mut params = CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_OPUS)
            .with_extra_data(Box::from(&b"not opus"[..]));
        assert_eq!(
            OggOpusDecoder::new(&params).err(),
            Some(OpusConfigError::InvalidHeader)
        );
    }

    #[test]
    fn codec_parameters_must_identify_an_ogg_opus_track() {
        let mut params = opus_params(2, 312, 312, 0, 0);
        params.channels = None;
        assert_eq!(
            OggOpusDecoder::new(&params).err(),
            Some(OpusConfigError::InvalidHeader)
        );
    }

    #[test]
    fn isobmff_dops_header_is_not_read_as_little_endian_ogg() {
        let mut params = opus_params(2, 312, 312, 0, 0);
        params.extra_data.as_mut().unwrap()[8] = 0;
        assert_eq!(
            OggOpusDecoder::new(&params).err(),
            Some(OpusConfigError::InvalidHeader)
        );
    }

    #[test]
    fn symphonia_demuxed_packets_decode_to_pcm() {
        let channels = 2;
        let frame_size = 960;
        let mut encoder =
            OpusEncoder::new(OPUS_SAMPLE_RATE as i32, channels, Application::Audio).unwrap();
        let head = OpusHead::for_encoder(&encoder, OPUS_SAMPLE_RATE);
        let pre_skip = usize::from(head.pre_skip);
        let mut writer = OggOpusWriter::new(Vec::new(), head).unwrap();
        // Put the final packet on its own page so the previous granule gives
        // the demuxer enough information to derive end padding.
        writer.set_page_target(1);
        let mut encoded = vec![0; MAX_PACKET_BYTES];
        let silent_frame = vec![0.0; frame_size * channels];
        let first_len = encoder
            .encode(&silent_frame, frame_size, &mut encoded)
            .unwrap();
        writer.write_packet(&encoded[..first_len]).unwrap();
        let second_len = encoder
            .encode(&silent_frame, frame_size, &mut encoded)
            .unwrap();
        let end_trim = 408usize;
        writer
            .write_packet_with_duration(
                &encoded[..second_len],
                u32::try_from(frame_size - end_trim).unwrap(),
            )
            .unwrap();
        let ogg = writer.finish().unwrap();

        let media = MediaSourceStream::new(Box::new(Cursor::new(ogg)), Default::default());
        let probed = symphonia::default::get_probe()
            .format(
                &Hint::new(),
                media,
                &FormatOptions {
                    enable_gapless: true,
                    ..FormatOptions::default()
                },
                &MetadataOptions::default(),
            )
            .unwrap();
        let mut format = probed.format;
        let track = format.default_track().unwrap();
        assert_eq!(track.codec_params.codec, CODEC_TYPE_OPUS);
        let track_id = track.id;
        let mut decoder = OggOpusDecoder::new(&track.codec_params).unwrap();
        let mut decoded_samples = 0;

        loop {
            match format.next_packet() {
                Ok(packet) if packet.track_id() == track_id => {
                    decoded_samples += decoder
                        .decode(packet.buf(), packet.trim_end())
                        .unwrap()
                        .samples
                        .len();
                }
                Ok(_) => {}
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(error) => panic!("unexpected demux error: {error}"),
            }
        }

        assert_eq!(
            decoded_samples,
            (2 * frame_size - pre_skip - end_trim) * channels
        );
    }
}
