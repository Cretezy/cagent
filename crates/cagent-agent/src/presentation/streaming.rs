const PLAN_START: &str = "<proposed_plan>";
const PLAN_END: &str = "</proposed_plan>";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposedPlanStreamEvent {
    Normal(String),
    PlanStart,
    PlanDelta(String),
    PlanEnd,
}

#[derive(Default)]
pub struct ProposedPlanStreamParser {
    buffer: String,
    in_plan: bool,
}

impl ProposedPlanStreamParser {
    pub fn push(&mut self, delta: &str) -> Vec<ProposedPlanStreamEvent> {
        self.buffer.push_str(delta);
        self.drain(false)
    }

    pub fn finish(&mut self) -> Vec<ProposedPlanStreamEvent> {
        self.drain(true)
    }

    fn drain(&mut self, finish: bool) -> Vec<ProposedPlanStreamEvent> {
        let mut events = Vec::new();
        loop {
            let marker = if self.in_plan { PLAN_END } else { PLAN_START };
            if let Some(index) = self.buffer.find(marker) {
                let text = self.buffer[..index].to_owned();
                if !text.is_empty() {
                    events.push(if self.in_plan {
                        ProposedPlanStreamEvent::PlanDelta(text)
                    } else {
                        ProposedPlanStreamEvent::Normal(text)
                    });
                }
                self.buffer.drain(..index + marker.len());
                self.in_plan = !self.in_plan;
                events.push(if self.in_plan {
                    ProposedPlanStreamEvent::PlanStart
                } else {
                    ProposedPlanStreamEvent::PlanEnd
                });
                continue;
            }

            let retained = if finish {
                0
            } else {
                longest_marker_prefix_suffix(&self.buffer, marker)
            };
            let emit_end = self.buffer.len().saturating_sub(retained);
            if emit_end > 0 {
                let text = self.buffer[..emit_end].to_owned();
                self.buffer.drain(..emit_end);
                events.push(if self.in_plan {
                    ProposedPlanStreamEvent::PlanDelta(text)
                } else {
                    ProposedPlanStreamEvent::Normal(text)
                });
            }
            if finish {
                if self.in_plan {
                    self.in_plan = false;
                    events.push(ProposedPlanStreamEvent::PlanEnd);
                }
                self.buffer.clear();
            }
            break;
        }
        events
    }
}

fn longest_marker_prefix_suffix(text: &str, marker: &str) -> usize {
    (1..marker.len())
        .rev()
        .find(|length| text.ends_with(&marker[..*length]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_plan_markup_across_transport_chunks() {
        let mut parser = ProposedPlanStreamParser::default();
        assert_eq!(
            parser.push("Intro\n<proposed_"),
            [ProposedPlanStreamEvent::Normal("Intro\n".into())]
        );
        assert_eq!(
            parser.push("plan># Build\n\n1. Ed"),
            [
                ProposedPlanStreamEvent::PlanStart,
                ProposedPlanStreamEvent::PlanDelta("# Build\n\n1. Ed".into()),
            ]
        );
        assert_eq!(
            parser.push("it</proposed_plan>After"),
            [
                ProposedPlanStreamEvent::PlanDelta("it".into()),
                ProposedPlanStreamEvent::PlanEnd,
                ProposedPlanStreamEvent::Normal("After".into()),
            ]
        );
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn finalizes_an_unclosed_plan_as_plan_content() {
        let mut parser = ProposedPlanStreamParser::default();
        let _ = parser.push("<proposed_plan>Plan");
        assert_eq!(parser.finish(), [ProposedPlanStreamEvent::PlanEnd]);
    }
}
