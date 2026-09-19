use std::io::Write as _;
use std::path::{Path, PathBuf};

pub(crate) const DIAGNOSTIC_LOG_BYTES: u64 = 8 * 1024 * 1024;
const DIAGNOSTIC_LOG_ARCHIVES: usize = 3;

pub(crate) struct DiagnosticLog {
    path: PathBuf,
    file: std::fs::File,
    bytes: u64,
}

impl DiagnosticLog {
    pub(crate) fn open(data_dir: &Path) -> std::io::Result<Self> {
        let path = data_dir.join("diagnostic.log");
        let bytes = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
        let mut log = Self {
            file: std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?,
            path,
            bytes,
        };
        if log.bytes >= DIAGNOSTIC_LOG_BYTES {
            log.rotate()?;
        }
        Ok(log)
    }

    fn archive_path(&self, index: usize) -> PathBuf {
        self.path.with_file_name(format!("diagnostic.log.{index}"))
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        for index in (1..=DIAGNOSTIC_LOG_ARCHIVES).rev() {
            let source = if index == 1 {
                self.path.clone()
            } else {
                self.archive_path(index - 1)
            };
            let destination = self.archive_path(index);
            if index == DIAGNOSTIC_LOG_ARCHIVES
                && let Err(error) = std::fs::remove_file(&destination)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error);
            }
            if let Err(error) = std::fs::rename(source, destination)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error);
            }
        }
        self.file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.bytes = 0;
        Ok(())
    }
}

impl std::io::Write for DiagnosticLog {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.bytes > 0 && self.bytes.saturating_add(buffer.len() as u64) > DIAGNOSTIC_LOG_BYTES {
            self.rotate()?;
        }
        let written = self.file.write(buffer)?;
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

pub(super) fn init_logging(
    data_dir: &Path,
) -> Result<tracing_appender::non_blocking::WorkerGuard, Box<dyn std::error::Error>> {
    let log = DiagnosticLog::open(data_dir)?;
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(8_192)
        .lossy(true)
        .finish(log);
    let cagent_log = std::env::var("CAGENT_LOG").ok();
    let rust_log = std::env::var("RUST_LOG").ok();
    let filter = logging_filter(cagent_log.as_deref(), rust_log.as_deref());
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(filter)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(writer)
        .try_init()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(guard)
}

pub(crate) const DEFAULT_LOG_FILTER: &str = "cagent=info,cagent_agent=info,cagent_cli=info";

pub(crate) fn logging_filter(
    cagent_log: Option<&str>,
    rust_log: Option<&str>,
) -> tracing_subscriber::EnvFilter {
    let selected = [cagent_log, rust_log]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .find(|filter| tracing_subscriber::EnvFilter::try_new(filter).is_ok())
        .unwrap_or(DEFAULT_LOG_FILTER);
    let mut filter = tracing_subscriber::EnvFilter::new(selected);
    for target in ["tokenize", "parse", "expansion", "notify"] {
        let explicitly_configured = selected.split(',').any(|directive| {
            directive
                .trim()
                .split_once('=')
                .is_some_and(|(configured, _)| configured.trim() == target)
        });
        if !explicitly_configured {
            filter = filter.add_directive(
                format!("{target}=info")
                    .parse()
                    .expect("static diagnostic directive should parse"),
            );
        }
    }
    filter
}
