//! DeepSeek Harness (DSH) 会话转录的底层读取辅助。
//!
//! DSH 把每个会话持久化为 `~/.dsh/sessions/<workspace-slug>/session-<id>/
//! session.vN.jsonl.zstd`（或未压缩的 `session.vN.jsonl`）。压缩产物是多个
//! 独立 Zstandard 帧的标准拼接：一个只含 header 行的帧，后跟每个 append
//! 批次一帧；解压后是逐行 JSON 事件（v0-v4 均为 JSONL，v4 事件词表见
//! `memory_ingest::read_dsh_session` 与 `telemetry_store::transcripts`）。
//!
//! 撕裂的末帧只贡献其中完整解出的行——与 DSH 自身的崩溃恢复语义一致：
//! 流中途遇到解压错误时调用方应停止迭代并保留已读内容，而不是整体失败。

use std::io::BufRead;
use std::path::Path;

/// 文件是否是 DSH 的 zstd 压缩转录（`*.jsonl.zstd`）。
pub(crate) fn is_zstd_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("zstd")
}

/// 打开一份 DSH 转录并返回按行读取器：zstd 文件走多帧流式解压，未压缩
/// 文件直接按行读。`Box<dyn BufRead>` 让调用方用同一套 `lines()` 循环
/// 消费两种编码；解压错误出现在迭代中（撕裂尾部），由调用方决定取舍。
pub(crate) fn open_lines(path: &Path) -> Result<Box<dyn BufRead>, String> {
    if is_zstd_path(path) {
        let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        // zstd::stream::read::Decoder 默认连续解码拼接的所有帧。
        let decoder =
            zstd::stream::read::Decoder::new(file).map_err(|error| error.to_string())?;
        Ok(Box::new(std::io::BufReader::new(decoder)))
    } else {
        let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        Ok(Box::new(std::io::BufReader::new(file)))
    }
}

/// DSH 会话目录里同一会话可能同时保留多个格式代际（`session.jsonl.zstd`、
/// `session.v1.jsonl.zstd` … 迁移保留源文件）。运行时只读数值最高的一代；
/// 扫描器据此跳过旧代，避免同一会话重复入账或被旧子集覆盖。
pub(crate) fn is_latest_generation(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    // session.v4.jsonl.zstd 的 file_stem 是 "session.v4"；再剥一层 jsonl。
    let stem = stem
        .strip_suffix(".jsonl")
        .map(str::to_string)
        .unwrap_or_else(|| stem.to_string());
    let Some(version) = generation_of(&stem) else {
        return false;
    };
    let Some(directory) = path.parent() else {
        return true;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return true;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(other_stem) = name
            .strip_suffix(".jsonl.zstd")
            .or_else(|| name.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let Some(other_version) = generation_of(other_stem) else {
            continue;
        };
        if other_version > version {
            return false;
        }
    }
    true
}

/// `session` → Some(0)，`session.v3` → Some(3)，其余（如 `session.lock`）→ None。
/// 传入的 stem 应已剥掉 `.jsonl`/`.jsonl.zstd` 后缀（`Path::file_stem` 只剥
/// 最后一层扩展名，`session.jsonl.zstd` 的 stem 是 `session.jsonl`）。
pub(crate) fn generation_of(stem: &str) -> Option<u32> {
    if stem == "session" {
        return Some(0);
    }
    stem.strip_prefix("session.v")
        .and_then(|suffix| suffix.parse::<u32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_parsing_covers_released_formats() {
        assert_eq!(generation_of("session"), Some(0));
        assert_eq!(generation_of("session.v1"), Some(1));
        assert_eq!(generation_of("session.v4"), Some(4));
        assert_eq!(generation_of("session.lock"), None);
        assert_eq!(generation_of("session.vX"), None);
    }

    #[test]
    fn latest_generation_ignores_older_siblings() {
        let directory = std::env::temp_dir().join(format!(
            "dsh-gen-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let v3 = directory.join("session.v3.jsonl.zstd");
        let v4 = directory.join("session.v4.jsonl.zstd");
        std::fs::write(&v3, []).unwrap();
        std::fs::write(&v4, []).unwrap();
        assert!(!is_latest_generation(&v3));
        assert!(is_latest_generation(&v4));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn multi_frame_zstd_decodes_all_batches() {
        let directory = std::env::temp_dir().join(format!(
            "dsh-zstd-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session.v4.jsonl.zstd");
        // 两个独立帧：header 行 + 一个事件行，模拟逐批追加的物理布局。
        let header_frame =
            zstd::encode_all(b"{\"type\":\"session\",\"version\":4}\n".as_slice(), 3).unwrap();
        let event_frame = zstd::encode_all(
            b"{\"type\":\"user/message\",\"seq\":1,\"data\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n".as_slice(),
            3,
        )
        .unwrap();
        let mut bytes = header_frame;
        bytes.extend_from_slice(&event_frame);
        std::fs::write(&path, bytes).unwrap();

        let reader = open_lines(&path).unwrap();
        let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("user/message"));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
