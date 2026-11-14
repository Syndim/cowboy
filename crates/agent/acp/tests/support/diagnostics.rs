use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

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
