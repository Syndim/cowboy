use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

const ISOLATED_CAPTURE_TEST: &str = "COWBOY_ACP_ISOLATED_CAPTURE_TEST";

pub async fn isolated_capture_test(name: &str) -> bool {
    if matches!(std::env::var(ISOLATED_CAPTURE_TEST).as_deref(), Ok(guard) if guard == name) {
        return false;
    }

    let output = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", name, "--nocapture"])
        .env(ISOLATED_CAPTURE_TEST, name)
        .output()
        .await
        .expect("run isolated capture test");
    assert!(
        output.status.success(),
        "isolated capture test {name} exited with {:?}",
        output.status.code()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed;")),
        "isolated capture test {name} did not run exactly one passing test"
    );
    true
}

#[derive(Clone, Default)]
pub struct DiagnosticCapture(Arc<Mutex<Vec<String>>>, Option<tracing::Level>);

impl DiagnosticCapture {
    pub fn at_level(level: tracing::Level) -> Self {
        Self(Arc::default(), Some(level))
    }

    pub fn text(&self) -> String {
        self.0.lock().unwrap().join("\n")
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={value:?}", field.name()));
    }
}

impl Subscriber for DiagnosticCapture {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.1.is_none_or(|level| *metadata.level() <= level)
    }

    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        let mut fields = Fields(format!("span={}", attributes.metadata().name()));
        attributes.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, record: &Record<'_>) {
        let mut fields = Fields("span_record".into());
        record.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields(event.metadata().level().to_string());
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}
