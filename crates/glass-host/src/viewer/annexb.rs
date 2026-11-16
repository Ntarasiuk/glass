//! Annex B (start-code delimited) H.264 and HEVC, as the host sends it, into what
//! VideoToolbox takes: the parameter sets on their own and length-prefixed NAL units.

/// The NAL units in `data`, without their start codes.
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (n, &(_, body)) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |s| s.0);
        // A four-byte start code's leading zero belongs to it, not to this unit.
        while end > body && data[end - 1] == 0 && n + 1 < starts.len() {
            end -= 1;
        }
        if end > body {
            out.push(&data[body..end]);
        }
    }
    out
}

pub fn nal_type(nal: &[u8], hevc: bool) -> u8 {
    match nal.first() {
        Some(b) if hevc => (b >> 1) & 0x3f,
        Some(b) => b & 0x1f,
        None => 0,
    }
}

/// Parameter sets (H.264: SPS 7, PPS 8; HEVC: VPS 32, SPS 33, PPS 34) and access unit
/// delimiters (9; 35) don't go in the sample.
fn is_param_or_aud(t: u8, hevc: bool) -> bool {
    if hevc { matches!(t, 32..=35) } else { matches!(t, 7..=9) }
}

/// The parameter sets a keyframe starts with, in the order VideoToolbox wants them
/// (H.264: SPS, PPS; HEVC: VPS, SPS, PPS), or `None` if one is missing.
pub fn parameter_sets(data: &[u8], hevc: bool) -> Option<Vec<&[u8]>> {
    let nals = nal_units(data);
    let find = |t: u8| nals.iter().copied().find(|n| nal_type(n, hevc) == t);
    if hevc { Some(vec![find(32)?, find(33)?, find(34)?]) } else { Some(vec![find(7)?, find(8)?]) }
}

/// The frame as length-prefixed (4-byte big-endian) NAL units, parameter sets left out.
pub fn to_length_prefixed(data: &[u8], hevc: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for nal in nal_units(data) {
        if is_param_or_aud(nal_type(nal, hevc), hevc) {
            continue;
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const H264_KEY: &[u8] = &[
        0, 0, 0, 1, 0x67, 0x64, 0, 0x33, // SPS
        0, 0, 0, 1, 0x68, 0xee, 0x3c, // PPS
        0, 0, 1, 0x65, 0x88, 0x80, 0, 0, // IDR slice (ends with zeros that are data)
    ];

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let nals = nal_units(H264_KEY);
        assert_eq!(nals, [&[0x67, 0x64, 0, 0x33][..], &[0x68, 0xee, 0x3c], &[0x65, 0x88, 0x80, 0, 0]]);
        assert_eq!(nals.iter().map(|n| nal_type(n, false)).collect::<Vec<_>>(), [7, 8, 5]);
    }

    #[test]
    fn finds_parameter_sets_and_prefixes_the_rest() {
        assert_eq!(parameter_sets(H264_KEY, false).unwrap(), [&[0x67, 0x64, 0, 0x33][..], &[0x68, 0xee, 0x3c]]);
        assert_eq!(to_length_prefixed(H264_KEY, false), [0, 0, 0, 5, 0x65, 0x88, 0x80, 0, 0]);
        // A delta has no parameter sets.
        assert!(parameter_sets(&[0, 0, 0, 1, 0x41, 1], false).is_none());
    }

    #[test]
    fn hevc_types() {
        let key = [0, 0, 0, 1, 0x40, 1, 0xaa, 0, 0, 0, 1, 0x42, 1, 0xbb, 0, 0, 0, 1, 0x44, 1, 0xcc, 0, 0, 0, 1, 0x26, 1, 0xdd];
        let sets = parameter_sets(&key, true).unwrap();
        assert_eq!(sets.iter().map(|n| nal_type(n, true)).collect::<Vec<_>>(), [32, 33, 34]);
        assert_eq!(to_length_prefixed(&key, true), [0, 0, 0, 3, 0x26, 1, 0xdd]);
    }
}
