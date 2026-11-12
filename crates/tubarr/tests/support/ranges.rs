//! Small range-response faults and an overflow producer whose EOF the test controls.
#![allow(dead_code)]

use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    body::{Body, Bytes},
    http::{StatusCode, header},
    response::Response,
};
use futures_util::stream;
use tokio::sync::watch;

#[derive(Clone)]
pub enum Fault {
    /// Placeholders use the actual clipped response bounds and source length.
    Headers(Vec<String>),
    UnknownTotal,
    Truncated,
    Excess,
    LengthMismatch,
    ShortLength,
    Overflow(Arc<Overflow>),
}

impl Fault {
    pub fn response(&self, data: &[u8], start: usize, end: usize) -> Response {
        let span = format!("bytes {start}-{end}/{}", data.len());
        let headers = match self {
            Self::Headers(values) => values
                .iter()
                .map(|v| {
                    v.replace("{start}", &start.to_string())
                        .replace("{end}", &end.to_string())
                        .replace("{total}", &data.len().to_string())
                })
                .collect(),
            Self::UnknownTotal => vec![format!("bytes {start}-{end}/*")],
            _ => vec![span],
        };
        let mut bytes = data[start..=end].to_vec();
        let body = match self {
            Self::Truncated => {
                bytes.pop();
                Body::from_stream(stream::iter([Ok::<_, Infallible>(Bytes::from(bytes))]))
            }
            Self::Excess => {
                bytes.push(0);
                Body::from_stream(stream::iter([Ok::<_, Infallible>(Bytes::from(bytes))]))
            }
            Self::LengthMismatch => {
                bytes.push(0);
                Body::from(bytes)
            }
            Self::ShortLength => {
                bytes.pop();
                Body::from(bytes)
            }
            Self::Overflow(control) => control.body(bytes),
            _ => Body::from(bytes),
        };
        let mut response = Response::builder().status(StatusCode::PARTIAL_CONTENT);
        for value in headers {
            response = response.header(header::CONTENT_RANGE, value);
        }
        response.body(body).unwrap()
    }
}

#[derive(Clone, Copy, Default)]
struct Progress {
    overflowed: usize,
    dropped: usize,
    completed: usize,
}

#[derive(Default)]
pub struct Overflow {
    progress: watch::Sender<Progress>,
    finish: watch::Sender<bool>,
}

struct Producer(Arc<Overflow>);

impl Drop for Producer {
    fn drop(&mut self) {
        self.0.progress.send_modify(|p| p.dropped += 1);
    }
}

impl Overflow {
    fn body(self: &Arc<Self>, bytes: Vec<u8>) -> Body {
        let producer = Producer(self.clone());
        let finish = self.finish.subscribe();
        Body::from_stream(stream::unfold(
            (0, Some(Bytes::from(bytes)), producer, finish),
            |(step, mut bytes, producer, mut finish)| async move {
                let chunk = match step {
                    0 => bytes.take().unwrap(),
                    1 => {
                        producer.0.progress.send_modify(|p| p.overflowed += 1);
                        Bytes::from_static(&[0])
                    }
                    _ => {
                        let _ = finish.wait_for(|ready| *ready).await;
                        producer.0.progress.send_modify(|p| p.completed += 1);
                        return None;
                    }
                };
                Some((
                    Ok::<_, Infallible>(chunk),
                    (step + 1, bytes, producer, finish),
                ))
            },
        ))
    }

    /// EOF stays unavailable. The deadline is a stuck-test guard, not a throughput assertion.
    pub async fn stopped_before_eof(&self, count: usize) -> bool {
        let mut progress = self.progress.subscribe();
        tokio::time::timeout(
            Duration::from_secs(5),
            progress.wait_for(|p| p.overflowed >= count && p.dropped >= count),
        )
        .await
        .is_ok_and(|result| result.is_ok_and(|p| p.completed == 0))
    }

    pub fn release(&self) {
        self.finish.send_replace(true);
    }
}
