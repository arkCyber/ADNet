# `a3net-media`

> 航空级短视频媒体管线 —— 摄取 → 转码 → 分段 → 音频 → DAG,每段 BLAKE3 寻址,DO-178C DAL-B 安全规范。
>
> Aerospace-grade short-video media pipeline — ingest → transcode → segment → audio → DAG, every segment BLAKE3-addressed, DO-178C DAL-B safety profile.

## 概览(Overview)

`a3net-media` 是 A3Net 中负责短视频(短片)处理的整条管线。它不是通用视频框架,而是面向"用户上传一段短视频到 NAS,后台做格式转换 + 分段 + 完整性校验"这一具体场景的工业级实现。设计上遵守 DO-178C DAL-B:每一个解码阶段的输出都带长度前缀与 BLAKE3 哈希,任何截断 / 篡改都会被 `verify_dag` 立刻发现。

管线由五个核心阶段组成:

1. **摄取 (`ingest`)** —— `MediaIngester` 把 raw PCM 音频 + 原始视频帧序列吃进来,产出 `IngestReport { manifest, segments, audio_energy }`。
2. **转码 (`transcode`)** —— `Transcoder` 把帧转成多档 `Variant`(如 480p / 720p / 1080p),支持 H.264 / H.265 / VP9 / AV1;可走 ffmpeg 后端,也能用纯 Rust `transcode_synthetic` 生成合成测试流。
3. **分段 (`segment`)** —— `Segmenter` 按固定时长(GOP 对齐)切成 `Vec<Segment>`,每段带 kind(Video/Audio)+ 字节长度。
4. **音频指纹 (`audio`)** —— 计算 PCM 帧窗口的能量指纹 + 静音比 + 平均 RMS,用于去重 / 静音跳过。
5. **DAG (`dag`)** —— `MediaDagBuilder` 把所有 variant + segment + audio 节点合成一棵内容寻址 DAG,根哈希 = `blake3("a3net-media-v1" || ...)`,用 `MediaDag::verify(root_hash)` 验证整段。

辅助模块:`ffmpeg` 提供 `FFmpegTranscoder` + `FFmpegLocator`,`ffmpeg_probe` 读取元数据,`integrity` 给独立的 segment / root hash 工具,`persist` 提供 `MediaStore` 把 DAG 落到 `a3net-blobstore::BlobStore`。

## 特性(Features)

- **`MediaIngester::default()`** + `ingest(samples, sample_format, channels, audio_codec, frames, video_codec, fps) -> IngestReport`。
- **`MediaConfig` + `VariantLadder` + `SegmenterConfig`**:全部可序列化,方便 TOML / JSON 配置。
- **`Transcoder` + `TranscodeInput` / `TranscodeOutput`**:统一接口,fake (`transcode_synthetic`) 与 ffmpeg (`FFmpegTranscoder`) 后端可切换。
- **`Segment` / `Segmenter`**:固定时长切片,每个 segment 单独 `SegmentDigest`。
- **`MediaDag` + `MediaDagBuilder`**:所有节点 BLAKE3 寻址,`verify_dag(&dag, &segments) -> VerifyReport` 全量校验。
- **`verify_manifest`**:校验 manifest 自身的字段完整性(不变式检查)。
- **`MediaStore`**:`AliasMap` + 写入 / 读取 / 删除 / 列出已入库的 manifest。
- **可选 `aerospace` feature**:打开后启用 `aerospace` 模块(SAFETY_REVISION / DAL_LEVEL / HAZARD_REGISTER_REV / 覆盖率目标),CI 用 `tests/aerospace_compliance.rs` 验证 DO-178C 流程。
- **零 unsafe**(`#![forbid(unsafe_code)]`),所有公开函数返回 `Result<_, MediaError>`,长度前缀保证解码边界。

## 安装(Installation)

工作空间内 path 依赖:

```toml
# 你的 crate 的 Cargo.toml
a3net-media = { workspace = true }
```

```rust
use a3net_media::{
    MediaIngester, MediaDag, MediaDagBuilder, MediaConfig, VariantLadder, SegmenterConfig,
    MediaStore, MediaStoreReport, AliasMap,
    codec::{AudioCodec, SampleFormat, VideoCodec, MediaKind},
    segment::{Segment, Segmenter, SegmentKind},
    transcode::{Frame, Transcoder},
    integrity::{SegmentDigest, MediaDigest, media_root_hash, segment_hash},
    verify::{VerifyReport, VerifyStatus, verify_dag, verify_manifest},
};
```

## 使用(Usage)

### 1. 摄取一段合成 4 秒音视频

```rust
use a3net_media::codec::{AudioCodec, SampleFormat, VideoCodec};
use a3net_media::ingest::MediaIngester;
use a3net_media::segment::SegmentKind;
use a3net_media::transcode::Frame;

let ingester = MediaIngester::default();
let samples = vec![0u8; (48_000u64 * 4 / 1_000 * 2 * 2) as usize];
let frames: Vec<Frame> = (0..120).map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0)).collect();

let report = ingester.ingest(
    samples, SampleFormat::S16, 2, AudioCodec::Aac,
    frames, VideoCodec::H264, 30,
).expect("ingest");
println!("manifest root: {}", report.manifest.root.as_hex());
```

### 2. 用 ffmpeg 转码

```rust
use a3net_media::ffmpeg::{FFmpegConfig, FFmpegTranscoder};
let cfg = FFmpegConfig::default();
let transcoder = FFmpegTranscoder::new(cfg)?;
let out = transcoder.transcode(&input)?;
```

### 3. 用纯 Rust 合成流做单元测试

```rust
use a3net_media::ffmpeg::transcode_synthetic;
let out = transcode_synthetic(b"raw-bytes")?;
```

### 4. 校验 DAG

```rust
use a3net_media::verify::verify_dag;
let report = verify_dag(&dag, &segments)?;
assert!(matches!(report.status, VerifyStatus::Ok));
```

### 5. 持久化到 blob store

```rust
use a3net_media::{MediaStore, MediaStoreReport};
let store = MediaStore::new(blob_store);
let r: MediaStoreReport = store.persist_alias("intro", &report.manifest)?;
println!("alias -> {}", r.alias);
```

## 应用案例(Use Cases / Examples)

- **朋友圈短视频发布**:`a3net-socialfeed` 收到附件上传时,调 `MediaIngester::ingest` 生成 DAG,把根哈希写到 `PostAttachment::blob_hash`。
- **NAS 离线转码**:夜间任务把 `*.mov` 转成 720p H.264 + 480p H.264 双档,各自 `Segmenter` 切片,DAG 落到 `a3net-blobstore` 走 CDN。
- **直播回放切片**:把 30 分钟直播按 4 秒分片写 DAG,前端按 segment 拉取做缓冲播放。
- **完整性审计**:运营周期跑 `verify_dag` 全量校验,损坏的 segment 走 `MediaStore` 修复(重新从备份源拉)。
- **航空认证闭环**(aerospace feature):合规测试套件跑 `tests/aerospace_compliance.rs`,确认 `SAFETY_REVISION` 与 `HAZARD_REGISTER_REV` 匹配。

## 许可(License)

MIT OR Apache-2.0