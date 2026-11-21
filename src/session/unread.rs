use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use tokio::task::JoinSet;

use super::{Sidebar, chat::ChatSession};
use crate::api::{Failure, SpkClient, UnreadState};

type Counts = HashMap<String, UnreadState>;

#[derive(PartialEq)]
pub(super) enum Mark {
    Visible,
    Read,
    Unread(String, u64),
}

#[derive(PartialEq)]
struct Write {
    channel_id: String,
    mark: Mark,
}

#[derive(Default)]
pub(super) struct UnreadSession {
    pub refreshes: JoinSet<Result<Counts, Failure>>,
    live_during_refresh: Counts,
    pub writes: JoinSet<Result<UnreadState, Failure>>,
    pending: VecDeque<Write>,
    writing: Option<Write>,
    changed_during_write: bool,
}

impl UnreadSession {
    pub fn refresh(&mut self, client: &SpkClient) {
        self.refreshes = JoinSet::new();
        self.live_during_refresh.clear();
        let client = client.clone();
        self.refreshes.spawn(async move { client.unread().await });
    }

    pub fn complete_refresh(&mut self, sidebar: &mut Sidebar, result: Result<Counts, Failure>) {
        let live = std::mem::take(&mut self.live_during_refresh);
        if let Ok(mut counts) = result {
            // A delayed response must preserve changes delivered while it was in flight.
            counts.extend(live);
            sidebar.unread = Arc::new(counts);
        }
    }

    pub fn update(&mut self, sidebar: &mut Sidebar, channel_id: String, state: UnreadState) {
        if self
            .writing
            .as_ref()
            .is_some_and(|write| write.channel_id == channel_id)
        {
            self.changed_during_write = true;
        }
        if !self.refreshes.is_empty() {
            self.live_during_refresh.insert(channel_id.clone(), state);
        }
        Arc::make_mut(&mut sidebar.unread).insert(channel_id, state);
    }

    pub fn mark(&mut self, sidebar: &mut Sidebar, channel_id: String, mark: Mark) {
        if mark != Mark::Visible {
            sidebar.read_notice = None;
        }
        let write = Write { channel_id, mark };
        if self.pending.back() != Some(&write) {
            self.pending.push_back(write);
        }
    }

    pub fn disconnected(&mut self) {
        self.pending.retain(|write| write.mark != Mark::Visible);
    }

    pub fn write_next(&mut self, client: &SpkClient) {
        if self.writing.is_some() {
            return;
        }
        let Some(write) = self.pending.pop_front() else {
            return;
        };
        let client = client.clone();
        let channel_id = write.channel_id.clone();
        let message_id = match &write.mark {
            Mark::Unread(id, _) => Some(id.clone()),
            _ => None,
        };
        self.writes.spawn(async move {
            client
                .update_read_point(&channel_id, message_id.as_deref())
                .await
        });
        self.writing = Some(write);
        self.changed_during_write = false;
    }

    pub fn complete_write(
        &mut self,
        sidebar: &mut Sidebar,
        chat: &mut ChatSession,
        result: Result<UnreadState, Failure>,
    ) {
        let Some(write) = self.writing.take() else {
            return;
        };
        match write.mark {
            Mark::Visible => {}
            Mark::Read if result.is_err() => sidebar.read_notice = Some("Could not mark as read."),
            Mark::Read => chat.clear_unread_marker(&write.channel_id),
            Mark::Unread(id, request) => {
                chat.finish_mark_unread(&write.channel_id, id, request, result.is_ok());
                if result.is_err() {
                    sidebar.read_notice = Some("Could not mark as unread.");
                }
            }
        }
        if let Ok(state) = result
            && !self.changed_during_write
        {
            self.update(sidebar, write.channel_id, state);
        }
    }
}
