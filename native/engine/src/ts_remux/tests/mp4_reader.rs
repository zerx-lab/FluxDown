//! 测试用最小 MP4 盒读取：逐层校验盒长度，取出断言需要的字段。

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().expect("4 bytes"))
}

fn be16(data: &[u8], at: usize) -> u16 {
    u16::from_be_bytes(data[at..at + 2].try_into().expect("2 bytes"))
}

/// 拆出 `data` 里依次排列的盒：(类型, 负载)。盒长度必须恰好铺满 `data`。
pub(super) fn boxes(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let size = be32(data, pos) as usize;
        assert!(
            size >= 8 && pos + size <= data.len(),
            "box size {size} at {pos} overruns {}",
            data.len()
        );
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().expect("box type");
        out.push((kind, &data[pos + 8..pos + size]));
        pos += size;
    }
    out
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data)
        .into_iter()
        .find(|(k, _)| k == kind)
        .map(|(_, payload)| payload)
}

fn require<'a>(data: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
    child(data, kind).unwrap_or_else(|| panic!("missing {}", String::from_utf8_lossy(kind)))
}

fn runs(payload: &[u8]) -> Vec<(u32, u32)> {
    let count = be32(payload, 4) as usize;
    assert_eq!(payload.len(), 8 + count * 8, "run table length");
    (0..count)
        .map(|i| (be32(payload, 8 + i * 8), be32(payload, 12 + i * 8)))
        .collect()
}

#[derive(Debug)]
pub(super) struct AudioEntry {
    pub channels: u16,
    pub sample_rate_field: u32,
    pub audio_specific_config: [u8; 2],
}

#[derive(Debug)]
pub(super) struct Trak {
    pub handler: [u8; 4],
    pub tkhd_duration: u32,
    pub timescale: u32,
    pub mdhd_duration: u32,
    pub stts: Vec<(u32, u32)>,
    pub ctts: Option<Vec<(u32, u32)>>,
    /// (segment_duration, media_time)
    pub edits: Vec<(u32, i32)>,
    pub sizes: Vec<u32>,
    pub chunk_offset: u32,
    pub audio: Option<AudioEntry>,
}

impl Trak {
    pub fn sample_count(&self) -> u32 {
        self.stts.iter().map(|&(count, _)| count).sum()
    }
}

#[derive(Debug)]
pub(super) struct Movie {
    pub mvhd_duration: u32,
    pub traks: Vec<Trak>,
}

impl Movie {
    pub fn track(&self, handler: &[u8; 4]) -> &Trak {
        self.traks
            .iter()
            .find(|trak| &trak.handler == handler)
            .unwrap_or_else(|| panic!("no {} track", String::from_utf8_lossy(handler)))
    }
}

fn parse_audio_entry(stsd: &[u8]) -> Option<AudioEntry> {
    let entries = boxes(&stsd[8..]);
    let (kind, mp4a) = entries.first()?;
    if kind != b"mp4a" {
        return None;
    }
    let esds = require(&mp4a[28..], b"esds");
    let at = esds
        .windows(2)
        .position(|w| w == [0x05, 0x02])
        .expect("DecoderSpecificInfo");
    Some(AudioEntry {
        channels: be16(mp4a, 16),
        sample_rate_field: be32(mp4a, 24),
        audio_specific_config: [esds[at + 2], esds[at + 3]],
    })
}

fn parse_trak(trak: &[u8]) -> Trak {
    let tkhd = require(trak, b"tkhd");
    let edits = child(trak, b"edts").map_or_else(Vec::new, |edts| {
        let elst = require(edts, b"elst");
        let count = be32(elst, 4) as usize;
        assert_eq!(elst.len(), 8 + count * 12, "elst length");
        (0..count)
            .map(|i| (be32(elst, 8 + i * 12), be32(elst, 12 + i * 12) as i32))
            .collect()
    });
    let mdia = require(trak, b"mdia");
    let mdhd = require(mdia, b"mdhd");
    let hdlr = require(mdia, b"hdlr");
    let stbl = require(require(mdia, b"minf"), b"stbl");
    let stsz = require(stbl, b"stsz");
    let sample_count = be32(stsz, 8) as usize;
    assert_eq!(stsz.len(), 12 + sample_count * 4, "stsz length");
    let stco = require(stbl, b"stco");
    assert_eq!(be32(stco, 4), 1, "single chunk");
    Trak {
        handler: hdlr[8..12].try_into().expect("handler"),
        tkhd_duration: be32(tkhd, 20),
        timescale: be32(mdhd, 12),
        mdhd_duration: be32(mdhd, 16),
        stts: runs(require(stbl, b"stts")),
        ctts: child(stbl, b"ctts").map(runs),
        edits,
        sizes: (0..sample_count).map(|i| be32(stsz, 12 + i * 4)).collect(),
        chunk_offset: be32(stco, 8),
        audio: parse_audio_entry(require(stbl, b"stsd")),
    }
}

/// 解析整份 MP4 并校验跨盒不变式：样本数在 stts / ctts / stsz 间一致，mdhd 时长 =
/// stts 之和，样本数据落在 mdat 内且两轨首尾相接，mdat 铺到文件尾。
pub(super) fn parse(file: &[u8]) -> Movie {
    let top = boxes(file);
    let kinds: Vec<[u8; 4]> = top.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(kinds, [*b"ftyp", *b"moov", *b"mdat"]);
    let moov = top[1].1;
    let mvhd = require(moov, b"mvhd");
    let traks: Vec<Trak> = boxes(moov)
        .into_iter()
        .filter(|(kind, _)| kind == b"trak")
        .map(|(_, payload)| parse_trak(payload))
        .collect();

    let mdat_start = file.len() - top[2].1.len();
    let mut expected_offset = mdat_start as u64;
    for trak in &traks {
        assert_eq!(
            trak.sample_count() as usize,
            trak.sizes.len(),
            "stts vs stsz"
        );
        if let Some(ctts) = &trak.ctts {
            let count: u32 = ctts.iter().map(|&(count, _)| count).sum();
            assert_eq!(count as usize, trak.sizes.len(), "ctts vs stsz");
        }
        let total: u64 = trak
            .stts
            .iter()
            .map(|&(c, d)| u64::from(c) * u64::from(d))
            .sum();
        assert_eq!(u64::from(trak.mdhd_duration), total, "mdhd vs stts");
        assert_eq!(
            u64::from(trak.chunk_offset),
            expected_offset,
            "chunk offset"
        );
        expected_offset += trak.sizes.iter().map(|&s| u64::from(s)).sum::<u64>();
    }
    assert_eq!(expected_offset, file.len() as u64, "samples fill mdat");
    Movie {
        mvhd_duration: be32(mvhd, 16),
        traks,
    }
}
