use std::cmp::Reverse;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use glob::Pattern;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextFilePayload {
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "numLines")]
    pub num_lines: usize,
    #[serde(rename = "startLine")]
    pub start_line: usize,
    #[serde(rename = "totalLines")]
    pub total_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    pub file: TextFilePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StructuredPatchHunk {
    #[serde(rename = "oldStart")]
    pub old_start: usize,
    #[serde(rename = "oldLines")]
    pub old_lines: usize,
    #[serde(rename = "newStart")]
    pub new_start: usize,
    #[serde(rename = "newLines")]
    pub new_lines: usize,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "originalFile")]
    pub original_file: Option<String>,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditFileOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "oldString")]
    pub old_string: String,
    #[serde(rename = "newString")]
    pub new_string: String,
    #[serde(rename = "originalFile")]
    pub original_file: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "userModified")]
    pub user_modified: bool,
    #[serde(rename = "replaceAll")]
    pub replace_all: bool,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobSearchOutput {
    #[serde(rename = "durationMs")]
    pub duration_ms: u128,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchInput {
    pub pattern: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    #[serde(rename = "output_mode")]
    pub output_mode: Option<String>,
    #[serde(rename = "-B")]
    pub before: Option<usize>,
    #[serde(rename = "-A")]
    pub after: Option<usize>,
    #[serde(rename = "-C")]
    pub context_short: Option<usize>,
    pub context: Option<usize>,
    #[serde(rename = "-n")]
    pub line_numbers: Option<bool>,
    #[serde(rename = "-i")]
    pub case_insensitive: Option<bool>,
    #[serde(rename = "type")]
    pub file_type: Option<String>,
    pub head_limit: Option<usize>,
    pub offset: Option<usize>,
    pub multiline: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchOutput {
    pub mode: Option<String>,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub content: Option<String>,
    #[serde(rename = "numLines")]
    pub num_lines: Option<usize>,
    #[serde(rename = "numMatches")]
    pub num_matches: Option<usize>,
    #[serde(rename = "appliedLimit")]
    pub applied_limit: Option<usize>,
    #[serde(rename = "appliedOffset")]
    pub applied_offset: Option<usize>,
}

pub fn read_file(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> io::Result<ReadFileOutput> {
    let cwd = std::env::current_dir()?;
    read_file_in_dir(&cwd, path, offset, limit)
}

/// 在显式基准目录中读取文本文件。
///
/// `base` 必须可规范化为已存在目录；相对 `path` 基于该目录解析。绝对 `path`
/// 可指向 `base` 之外，但目标必须存在。
pub fn read_file_in_dir(
    base: &Path,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> io::Result<ReadFileOutput> {
    let absolute_path = normalize_path_from(base, path)?;
    let content = fs::read_to_string(&absolute_path)?;
    let lines: Vec<&str> = content.lines().collect();
    let start_index = offset.unwrap_or(0).min(lines.len());
    let end_index = limit.map_or(lines.len(), |limit| {
        start_index.saturating_add(limit).min(lines.len())
    });
    let selected = lines[start_index..end_index].join("\n");

    Ok(ReadFileOutput {
        kind: String::from("text"),
        file: TextFilePayload {
            file_path: absolute_path.to_string_lossy().into_owned(),
            content: selected,
            num_lines: end_index.saturating_sub(start_index),
            start_line: start_index.saturating_add(1),
            total_lines: lines.len(),
        },
    })
}

pub fn write_file(path: &str, content: &str) -> io::Result<WriteFileOutput> {
    let cwd = std::env::current_dir()?;
    write_file_in_dir(&cwd, path, content)
}

/// 在显式基准目录中写入文件，并创建缺失的父目录。
///
/// `base` 必须可规范化为已存在目录；相对 `path` 基于该目录解析并返回稳定的绝对路径。
/// 绝对 `path` 可指向 `base` 之外，多级缺失父目录会在写入时创建。
pub fn write_file_in_dir(base: &Path, path: &str, content: &str) -> io::Result<WriteFileOutput> {
    let absolute_path = normalize_path_allow_missing_from(base, path)?;
    let original_file = fs::read_to_string(&absolute_path).ok();
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&absolute_path, content)?;

    Ok(WriteFileOutput {
        kind: if original_file.is_some() {
            String::from("update")
        } else {
            String::from("create")
        },
        file_path: absolute_path.to_string_lossy().into_owned(),
        content: content.to_owned(),
        structured_patch: make_patch(original_file.as_deref().unwrap_or(""), content),
        original_file,
        git_diff: None,
    })
}

pub fn edit_file(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> io::Result<EditFileOutput> {
    let cwd = std::env::current_dir()?;
    edit_file_in_dir(&cwd, path, old_string, new_string, replace_all)
}

/// 在显式基准目录中编辑已存在文件。
///
/// `base` 必须可规范化为已存在目录；相对 `path` 基于该目录解析。绝对 `path`
/// 可指向 `base` 之外，但目标必须存在。
pub fn edit_file_in_dir(
    base: &Path,
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> io::Result<EditFileOutput> {
    let absolute_path = normalize_path_from(base, path)?;
    let original_file = fs::read_to_string(&absolute_path)?;
    if old_string == new_string {
        // 文件内容已经是目标状态，无需修改，直接返回成功
        return Ok(EditFileOutput {
            file_path: absolute_path.to_string_lossy().into_owned(),
            old_string: old_string.to_owned(),
            new_string: new_string.to_owned(),
            original_file: original_file.clone(),
            structured_patch: vec![],
            user_modified: false,
            replace_all,
            git_diff: None,
        });
    }
    if !original_file.contains(old_string) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "old_string not found in file",
        ));
    }

    let updated = if replace_all {
        original_file.replace(old_string, new_string)
    } else {
        original_file.replacen(old_string, new_string, 1)
    };
    fs::write(&absolute_path, &updated)?;

    Ok(EditFileOutput {
        file_path: absolute_path.to_string_lossy().into_owned(),
        old_string: old_string.to_owned(),
        new_string: new_string.to_owned(),
        original_file: original_file.clone(),
        structured_patch: make_patch(&original_file, &updated),
        user_modified: false,
        replace_all,
        git_diff: None,
    })
}

pub fn glob_search(pattern: &str, path: Option<&str>) -> io::Result<GlobSearchOutput> {
    let cwd = std::env::current_dir()?;
    glob_search_in_dir(&cwd, pattern, path)
}

/// 在显式基准目录中执行 glob 搜索。
///
/// `base` 必须可规范化为已存在目录；省略 `path` 时直接搜索该目录，相对 `path`
/// 基于该目录解析。绝对 `path` 或绝对 `pattern` 可指向 `base` 之外。
pub fn glob_search_in_dir(
    base: &Path,
    pattern: &str,
    path: Option<&str>,
) -> io::Result<GlobSearchOutput> {
    let started = Instant::now();
    let normalized_base = normalize_base(base)?;
    let base_dir = path
        .map(|path| normalize_path_from_normalized_base(&normalized_base, path))
        .transpose()?
        .unwrap_or(normalized_base);
    let search_pattern = if Path::new(pattern).is_absolute() {
        pattern.to_owned()
    } else {
        base_dir.join(pattern).to_string_lossy().into_owned()
    };

    let mut matches = Vec::new();
    let entries = glob::glob(&search_pattern)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    for entry in entries.flatten() {
        if entry.is_file() {
            matches.push(entry);
        }
    }

    matches.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(Reverse)
    });

    let truncated = matches.len() > 100;
    let filenames = matches
        .into_iter()
        .take(100)
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    Ok(GlobSearchOutput {
        duration_ms: started.elapsed().as_millis(),
        num_files: filenames.len(),
        filenames,
        truncated,
    })
}

#[allow(clippy::too_many_lines)]
pub fn grep_search(input: &GrepSearchInput) -> io::Result<GrepSearchOutput> {
    let cwd = std::env::current_dir()?;
    grep_search_in_dir(&cwd, input)
}

/// 在显式基准目录中执行 grep 搜索。
///
/// `base` 必须可规范化为已存在目录；`input.path` 为空时搜索该目录，相对路径基于该
/// 目录解析。绝对 `input.path` 可指向 `base` 之外。
#[allow(clippy::too_many_lines)]
pub fn grep_search_in_dir(base: &Path, input: &GrepSearchInput) -> io::Result<GrepSearchOutput> {
    let started = Instant::now();
    let deadline = started + std::time::Duration::from_secs(GREP_SEARCH_TIMEOUT_SECS);
    let normalized_base = normalize_base(base)?;

    let base_path = input
        .path
        .as_deref()
        .map(|path| normalize_path_from_normalized_base(&normalized_base, path))
        .transpose()?
        .unwrap_or(normalized_base);

    let regex = RegexBuilder::new(&input.pattern)
        .case_insensitive(input.case_insensitive.unwrap_or(false))
        .dot_matches_new_line(input.multiline.unwrap_or(false))
        .build()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;

    let glob_filter = input
        .glob
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let file_type = input.file_type.as_deref();
    let output_mode = input
        .output_mode
        .clone()
        .unwrap_or_else(|| String::from("files_with_matches"));
    let context = input.context.or(input.context_short).unwrap_or(0);

    let mut filenames = Vec::new();
    let mut content_lines = Vec::new();
    let mut total_matches = 0usize;

    for file_path in collect_search_files(&base_path)? {
        // 超时检查：超过限制则返回已收集到的部分结果
        if Instant::now() > deadline {
            break;
        }

        if !matches_optional_filters(&file_path, glob_filter.as_ref(), file_type) {
            continue;
        }

        // 二进制文件跳过
        if is_binary_file(&file_path) {
            continue;
        }

        let Ok(file_contents) = fs::read_to_string(&file_path) else {
            continue;
        };

        if output_mode == "count" {
            let count = regex.find_iter(&file_contents).count();
            if count > 0 {
                filenames.push(file_path.to_string_lossy().into_owned());
                total_matches += count;
            }
            continue;
        }

        let lines: Vec<&str> = file_contents.lines().collect();
        let mut matched_lines = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if regex.is_match(line) {
                total_matches += 1;
                matched_lines.push(index);
            }
        }

        if matched_lines.is_empty() {
            continue;
        }

        filenames.push(file_path.to_string_lossy().into_owned());
        if output_mode == "content" {
            for index in matched_lines {
                let start = index.saturating_sub(input.before.unwrap_or(context));
                let end = (index + input.after.unwrap_or(context) + 1).min(lines.len());
                for (current, line) in lines.iter().enumerate().take(end).skip(start) {
                    let prefix = if input.line_numbers.unwrap_or(true) {
                        format!("{}:{}:", file_path.to_string_lossy(), current + 1)
                    } else {
                        format!("{}:", file_path.to_string_lossy())
                    };
                    content_lines.push(format!("{prefix}{line}"));
                }
            }
        }
    }

    let (filenames, applied_limit, applied_offset) =
        apply_limit(filenames, input.head_limit, input.offset);
    let content_output = if output_mode == "content" {
        let (lines, limit, offset) = apply_limit(content_lines, input.head_limit, input.offset);
        return Ok(GrepSearchOutput {
            mode: Some(output_mode),
            num_files: filenames.len(),
            filenames,
            num_lines: Some(lines.len()),
            content: Some(lines.join("\n")),
            num_matches: None,
            applied_limit: limit,
            applied_offset: offset,
        });
    } else {
        None
    };

    Ok(GrepSearchOutput {
        mode: Some(output_mode.clone()),
        num_files: filenames.len(),
        filenames,
        content: content_output,
        num_lines: None,
        num_matches: (output_mode == "count").then_some(total_matches),
        applied_limit,
        applied_offset,
    })
}

fn collect_search_files(base_path: &Path) -> io::Result<Vec<PathBuf>> {
    if base_path.is_file() {
        return Ok(vec![base_path.to_path_buf()]);
    }

    let mut files = Vec::new();
    let walker = WalkBuilder::new(base_path)
        .hidden(true) // 跳过隐藏文件/目录
        .git_ignore(true) // 尊重 .gitignore
        .git_global(true) // 尊重全局 gitignore
        .git_exclude(true) // 尊重 .git/info/exclude
        .ignore(true) // 尊重 .ignore
        .build();

    for entry in walker {
        let entry = entry.map_err(|error| io::Error::other(error.to_string()))?;
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            files.push(entry.path().to_path_buf());
        }
    }
    Ok(files)
}

/// 检测文件是否为二进制文件（前 8KB 包含 NUL 字节则视为二进制）
fn is_binary_file(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return true;
    };
    let mut buf = [0u8; 8192];
    let n = file.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0)
}

/// grep_search 的最大执行时间（秒）
const GREP_SEARCH_TIMEOUT_SECS: u64 = 30;

fn matches_optional_filters(
    path: &Path,
    glob_filter: Option<&Pattern>,
    file_type: Option<&str>,
) -> bool {
    if let Some(glob_filter) = glob_filter {
        let path_string = path.to_string_lossy();
        if !glob_filter.matches(&path_string) && !glob_filter.matches_path(path) {
            return false;
        }
    }

    if let Some(file_type) = file_type {
        let extension = path.extension().and_then(|extension| extension.to_str());
        if extension != Some(file_type) {
            return false;
        }
    }

    true
}

fn apply_limit<T>(
    items: Vec<T>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> (Vec<T>, Option<usize>, Option<usize>) {
    let offset_value = offset.unwrap_or(0);
    let mut items = items.into_iter().skip(offset_value).collect::<Vec<_>>();
    let explicit_limit = limit.unwrap_or(250);
    if explicit_limit == 0 {
        return (items, None, (offset_value > 0).then_some(offset_value));
    }

    let truncated = items.len() > explicit_limit;
    items.truncate(explicit_limit);
    (
        items,
        truncated.then_some(explicit_limit),
        (offset_value > 0).then_some(offset_value),
    )
}

fn make_patch(original: &str, updated: &str) -> Vec<StructuredPatchHunk> {
    let mut lines = Vec::new();
    for line in original.lines() {
        lines.push(format!("-{line}"));
    }
    for line in updated.lines() {
        lines.push(format!("+{line}"));
    }

    vec![StructuredPatchHunk {
        old_start: 1,
        old_lines: original.lines().count(),
        new_start: 1,
        new_lines: updated.lines().count(),
        lines,
    }]
}

fn normalize_path_from(base: &Path, path: &str) -> io::Result<PathBuf> {
    let normalized_base = normalize_base(base)?;
    normalize_path_from_normalized_base(&normalized_base, path)
}

fn normalize_base(base: &Path) -> io::Result<PathBuf> {
    let normalized = base.canonicalize()?;
    if !normalized.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("基准路径不是目录: {}", normalized.display()),
        ));
    }
    Ok(normalized)
}

fn normalize_path_from_normalized_base(base: &Path, path: &str) -> io::Result<PathBuf> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        base.join(path)
    };
    candidate.canonicalize()
}

fn normalize_path_allow_missing_from(base: &Path, path: &str) -> io::Result<PathBuf> {
    let normalized_base = normalize_base(base)?;
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        normalized_base.join(path)
    };

    match candidate.canonicalize() {
        Ok(canonical) => return Ok(canonical),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    for ancestor in candidate.ancestors().skip(1) {
        match ancestor.canonicalize() {
            Ok(canonical_ancestor) => {
                let suffix = candidate.strip_prefix(ancestor).map_err(|error| {
                    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
                })?;
                return append_normalized_suffix(canonical_ancestor, suffix);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("找不到可解析的路径祖先: {}", candidate.display()),
    ))
}

fn append_normalized_suffix(mut base: PathBuf, suffix: &Path) -> io::Result<PathBuf> {
    for component in suffix.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !base.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "路径不能越过文件系统根目录",
                    ));
                }
            }
            std::path::Component::Normal(component) => base.push(component),
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "缺失路径后缀必须是相对路径",
                ));
            }
        }
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        edit_file, edit_file_in_dir, glob_search, glob_search_in_dir, grep_search,
        grep_search_in_dir, read_file, read_file_in_dir, write_file, write_file_in_dir,
        GrepSearchInput,
    };

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            Self::new_in(&std::env::temp_dir(), name)
        }

        fn new_in(parent: &Path, name: &str) -> Self {
            let counter = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time should move forward")
                .as_nanos();
            let path = parent.join(format!(
                "clawd-native-{name}-{}-{counter}-{timestamp}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("test directory should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn temp_file(workspace: &TestDir, name: &str) -> PathBuf {
        workspace.path().join(name)
    }

    fn relative_to_current_dir(path: &Path) -> PathBuf {
        let current_dir = std::env::current_dir().expect("current directory should be readable");
        path.strip_prefix(current_dir)
            .expect("test path should be inside the current directory")
            .to_path_buf()
    }

    fn search_input(pattern: &str) -> GrepSearchInput {
        GrepSearchInput {
            pattern: pattern.to_owned(),
            path: None,
            glob: Some(String::from("*.txt")),
            output_mode: Some(String::from("files_with_matches")),
            before: None,
            after: None,
            context_short: None,
            context: None,
            line_numbers: None,
            case_insensitive: None,
            file_type: None,
            head_limit: None,
            offset: None,
            multiline: None,
        }
    }

    fn canonical_result_path(path: &str) -> PathBuf {
        PathBuf::from(path)
            .canonicalize()
            .expect("result path should canonicalize")
    }

    #[test]
    fn reads_and_writes_files() {
        let workspace = TestDir::new("read-write");
        let path = temp_file(&workspace, "read-write.txt");
        let write_output = write_file(path.to_string_lossy().as_ref(), "one\ntwo\nthree")
            .expect("write should succeed");
        assert_eq!(write_output.kind, "create");

        let read_output = read_file(path.to_string_lossy().as_ref(), Some(1), Some(1))
            .expect("read should succeed");
        assert_eq!(read_output.file.content, "two");
    }

    #[test]
    fn edits_file_contents() {
        let workspace = TestDir::new("edit");
        let path = temp_file(&workspace, "edit.txt");
        write_file(path.to_string_lossy().as_ref(), "alpha beta alpha")
            .expect("initial write should succeed");
        let output = edit_file(path.to_string_lossy().as_ref(), "alpha", "omega", true)
            .expect("edit should succeed");
        assert!(output.replace_all);
    }

    #[test]
    fn globs_and_greps_directory() {
        let workspace = TestDir::new("search-dir");
        let dir = workspace.path();
        let file = dir.join("demo.rs");
        write_file(
            file.to_string_lossy().as_ref(),
            "fn main() {\n println!(\"hello\");\n}\n",
        )
        .expect("file write should succeed");

        let globbed = glob_search("**/*.rs", Some(dir.to_string_lossy().as_ref()))
            .expect("glob should succeed");
        assert_eq!(globbed.num_files, 1);

        let grep_output = grep_search(&GrepSearchInput {
            pattern: String::from("hello"),
            path: Some(dir.to_string_lossy().into_owned()),
            glob: Some(String::from("**/*.rs")),
            output_mode: Some(String::from("content")),
            before: None,
            after: None,
            context_short: None,
            context: None,
            line_numbers: Some(true),
            case_insensitive: Some(false),
            file_type: None,
            head_limit: Some(10),
            offset: Some(0),
            multiline: Some(false),
        })
        .expect("grep should succeed");
        assert!(grep_output.content.unwrap_or_default().contains("hello"));
    }

    #[test]
    fn explicit_directory_read_and_write_are_isolated() {
        let workspace_a = TestDir::new("explicit-directory-read-write-a");
        let workspace_b = TestDir::new("explicit-directory-read-write-b");
        std::fs::write(workspace_a.path().join("same.txt"), "from-a")
            .expect("workspace_a file should be written");
        std::fs::write(workspace_b.path().join("same.txt"), "from-b")
            .expect("workspace_b file should be written");

        let a = read_file_in_dir(workspace_a.path(), "same.txt", None, None)
            .expect("workspace_a file should be read");
        let b = read_file_in_dir(workspace_b.path(), "same.txt", None, None)
            .expect("workspace_b file should be read");
        assert_eq!(a.file.content, "from-a");
        assert_eq!(b.file.content, "from-b");

        write_file_in_dir(workspace_a.path(), "created.txt", "only-a")
            .expect("workspace_a file should be created");
        assert!(workspace_a.path().join("created.txt").is_file());
        assert!(!workspace_b.path().join("created.txt").exists());
    }

    #[test]
    fn explicit_directory_edit_is_isolated() {
        let workspace_a = TestDir::new("explicit-directory-edit-a");
        let workspace_b = TestDir::new("explicit-directory-edit-b");
        std::fs::write(workspace_a.path().join("same.txt"), "from-a")
            .expect("workspace_a file should be written");
        std::fs::write(workspace_b.path().join("same.txt"), "from-b")
            .expect("workspace_b file should be written");

        edit_file_in_dir(workspace_a.path(), "same.txt", "from-a", "edited-a", false)
            .expect("workspace_a file should be edited");

        assert_eq!(
            std::fs::read_to_string(workspace_a.path().join("same.txt"))
                .expect("workspace_a file should be readable"),
            "edited-a"
        );
        assert_eq!(
            std::fs::read_to_string(workspace_b.path().join("same.txt"))
                .expect("workspace_b file should be readable"),
            "from-b"
        );
    }

    #[test]
    fn explicit_directory_concurrent_searches_are_isolated() {
        let original_cwd = std::env::current_dir().expect("current directory should be readable");
        let workspace_a = TestDir::new("explicit-directory-search-a");
        let workspace_b = TestDir::new("explicit-directory-search-b");
        std::fs::write(workspace_a.path().join("same.txt"), "from-a")
            .expect("workspace_a file should be written");
        std::fs::write(workspace_b.path().join("same.txt"), "from-b")
            .expect("workspace_b file should be written");

        let workspace_a_for_thread = workspace_a.path().to_path_buf();
        let thread_a = std::thread::spawn(move || {
            let read = read_file_in_dir(&workspace_a_for_thread, "same.txt", None, None)
                .expect("workspace_a file should be read");
            let glob = glob_search_in_dir(&workspace_a_for_thread, "*.txt", None)
                .expect("workspace_a glob should succeed");
            let grep = grep_search_in_dir(&workspace_a_for_thread, &search_input("from-a"))
                .expect("workspace_a grep should succeed");
            (read, glob, grep)
        });

        let workspace_b_for_thread = workspace_b.path().to_path_buf();
        let thread_b = std::thread::spawn(move || {
            let read = read_file_in_dir(&workspace_b_for_thread, "same.txt", None, None)
                .expect("workspace_b file should be read");
            let glob = glob_search_in_dir(&workspace_b_for_thread, "*.txt", None)
                .expect("workspace_b glob should succeed");
            let grep = grep_search_in_dir(&workspace_b_for_thread, &search_input("from-b"))
                .expect("workspace_b grep should succeed");
            (read, glob, grep)
        });

        let (read_a, glob_a, grep_a) = thread_a.join().expect("workspace_a thread should finish");
        let (read_b, glob_b, grep_b) = thread_b.join().expect("workspace_b thread should finish");
        let expected_a = workspace_a
            .path()
            .join("same.txt")
            .canonicalize()
            .expect("workspace_a file should canonicalize");
        let expected_b = workspace_b
            .path()
            .join("same.txt")
            .canonicalize()
            .expect("workspace_b file should canonicalize");

        assert_eq!(read_a.file.content, "from-a");
        assert_eq!(glob_a.num_files, 1);
        assert_eq!(
            std::path::PathBuf::from(&glob_a.filenames[0])
                .canonicalize()
                .expect("workspace_a glob result should canonicalize"),
            expected_a
        );
        assert_eq!(grep_a.num_files, 1);
        assert_eq!(
            std::path::PathBuf::from(&grep_a.filenames[0])
                .canonicalize()
                .expect("workspace_a grep result should canonicalize"),
            expected_a
        );
        assert_eq!(read_b.file.content, "from-b");
        assert_eq!(glob_b.num_files, 1);
        assert_eq!(
            std::path::PathBuf::from(&glob_b.filenames[0])
                .canonicalize()
                .expect("workspace_b glob result should canonicalize"),
            expected_b
        );
        assert_eq!(grep_b.num_files, 1);
        assert_eq!(
            std::path::PathBuf::from(&grep_b.filenames[0])
                .canonicalize()
                .expect("workspace_b grep result should canonicalize"),
            expected_b
        );
        assert_eq!(
            std::env::current_dir().expect("current directory should remain readable"),
            original_cwd
        );
    }

    #[test]
    fn explicit_relative_base_is_canonicalized_for_read_and_search() {
        let current_dir = std::env::current_dir().expect("current directory should be readable");
        let root = TestDir::new_in(&current_dir, "explicit-relative-base");
        let workspace_a = root.path().join("workspace-a");
        let workspace_b = root.path().join("workspace-b");
        std::fs::create_dir_all(&workspace_a).expect("workspace_a should be created");
        std::fs::create_dir_all(&workspace_b).expect("workspace_b should be created");
        std::fs::write(workspace_a.join("same.txt"), "from-a")
            .expect("workspace_a file should be written");
        std::fs::write(workspace_b.join("same.txt"), "from-b")
            .expect("workspace_b file should be written");
        let relative_base = relative_to_current_dir(&workspace_a);
        let expected = workspace_a
            .join("same.txt")
            .canonicalize()
            .expect("workspace_a file should canonicalize");

        let read = read_file_in_dir(&relative_base, "same.txt", None, None)
            .expect("relative-base read should succeed");
        let glob = glob_search_in_dir(&relative_base, "*.txt", None)
            .expect("relative-base glob should succeed");
        let grep = grep_search_in_dir(&relative_base, &search_input("from-a"))
            .expect("relative-base grep should succeed");

        assert_eq!(PathBuf::from(read.file.file_path), expected);
        assert_eq!(glob.num_files, 1);
        assert!(Path::new(&glob.filenames[0]).is_absolute());
        assert_eq!(canonical_result_path(&glob.filenames[0]), expected);
        assert_eq!(grep.num_files, 1);
        assert!(Path::new(&grep.filenames[0]).is_absolute());
        assert_eq!(canonical_result_path(&grep.filenames[0]), expected);
    }

    #[test]
    fn explicit_directory_write_normalizes_missing_parents_and_dot_segments() {
        let current_dir = std::env::current_dir().expect("current directory should be readable");
        let workspace = TestDir::new_in(&current_dir, "explicit-missing-parents");
        let relative_base = relative_to_current_dir(workspace.path());

        let output = write_file_in_dir(
            &relative_base,
            "missing/one/../two/./created.txt",
            "created",
        )
        .expect("multi-level missing path should be written");
        let expected = workspace
            .path()
            .join("missing/two/created.txt")
            .canonicalize()
            .expect("written file should canonicalize");

        assert!(Path::new(&output.file_path).is_absolute());
        assert_eq!(PathBuf::from(output.file_path), expected);
        assert_eq!(
            std::fs::read_to_string(expected).expect("written file should be readable"),
            "created"
        );
    }

    #[test]
    fn explicit_directory_allows_absolute_target_outside_base() {
        let workspace = TestDir::new("explicit-absolute-base");
        let outside = TestDir::new("explicit-absolute-target");
        let target = outside.path().join("nested/absolute.txt");

        let output = write_file_in_dir(
            workspace.path(),
            target.to_string_lossy().as_ref(),
            "outside-base",
        )
        .expect("absolute target should be written");
        let expected = target
            .canonicalize()
            .expect("absolute target should canonicalize");

        assert_eq!(PathBuf::from(output.file_path), expected);
        assert_eq!(
            std::fs::read_to_string(expected).expect("absolute target should be readable"),
            "outside-base"
        );
        assert!(!workspace.path().join("nested/absolute.txt").exists());
    }
}
