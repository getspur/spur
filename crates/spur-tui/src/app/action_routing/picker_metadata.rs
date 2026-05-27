use super::*;

impl App {
    pub(super) fn process_picker_metadata(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::ToggleSessionPin { session_id } => {
                if !self.tombstone_undo_replay {
                    let will_pin = !self
                        .metadata_store
                        .entry(&session_id)
                        .is_some_and(|entry| entry.pinned);
                    let label = if will_pin {
                        format!("Pinned '{}'", session_id)
                    } else {
                        format!("Unpinned '{}'", session_id)
                    };
                    let now = Instant::now();
                    let inverse = Action::ToggleSessionPin {
                        session_id: session_id.clone(),
                    };
                    self.tombstones.install(Tombstone {
                        view: ViewId::SessionPicker,
                        kind: TombstoneKind::Reversible { inverse },
                        label: label.clone(),
                        created_at: now,
                        expires_at: now + Duration::from_secs(60),
                    });
                    self.flash_hint(
                        format!("{} — press u to undo", label),
                        Duration::from_secs(2),
                    );
                }
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.pinned = !entry.pinned;
                self.persist_metadata("pin toggle");
                self.refresh_picker_metadata();
                self.dirty = true;
                None
            }

            Action::ToggleSessionArchive {
                session_id,
                via_legacy_key,
            } => {
                let show_legacy_archive_hint = via_legacy_key && !self.legacy_archive_hint_shown;
                if show_legacy_archive_hint {
                    self.legacy_archive_hint_shown = true;
                }
                if !self.tombstone_undo_replay {
                    let will_archive = !self
                        .metadata_store
                        .entry(&session_id)
                        .is_some_and(|entry| entry.archived);
                    let label = if will_archive {
                        format!("Archived '{}'", session_id)
                    } else {
                        format!("Restored '{}'", session_id)
                    };
                    let now = Instant::now();
                    let inverse = Action::ToggleSessionArchive {
                        session_id: session_id.clone(),
                        via_legacy_key: false,
                    };
                    self.tombstones.install(Tombstone {
                        view: ViewId::SessionPicker,
                        kind: TombstoneKind::Reversible { inverse },
                        label: label.clone(),
                        created_at: now,
                        expires_at: now + Duration::from_secs(60),
                    });
                    if !show_legacy_archive_hint {
                        self.flash_hint(
                            format!("{} — press u to undo", label),
                            Duration::from_secs(2),
                        );
                    }
                }
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.archived = !entry.archived;
                self.persist_metadata("archive toggle");
                self.refresh_picker_metadata();
                if show_legacy_archive_hint {
                    self.flash_hint_short(LEGACY_ARCHIVE_HINT);
                }
                self.dirty = true;
                None
            }

            Action::ToggleShowArchived => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.toggle_show_archived(&self.synopsis);
                }
                self.dirty = true;
                None
            }

            Action::RenameSession {
                ref session_id,
                ref new_title,
                ref original_title,
            } => {
                if !self.tombstone_undo_replay {
                    let label = format!("Renamed '{}' → '{}'", original_title, new_title);
                    let now = Instant::now();
                    let inverse = Action::RenameSession {
                        session_id: session_id.clone(),
                        new_title: original_title.clone(),
                        original_title: new_title.clone(),
                    };
                    self.tombstones.install(Tombstone {
                        view: ViewId::SessionPicker,
                        kind: TombstoneKind::Reversible { inverse },
                        label: label.clone(),
                        created_at: now,
                        expires_at: now + Duration::from_secs(60),
                    });
                    self.flash_hint(
                        format!("{} — press u to undo", label),
                        Duration::from_secs(2),
                    );
                }
                let entry = self.metadata_store.entry_mut(session_id);
                entry.title_override = if new_title.trim().is_empty() {
                    None
                } else {
                    Some(new_title.clone())
                };
                self.persist_metadata("rename");
                self.refresh_picker_metadata();
                self.dirty = true;
                None
            }

            Action::SaveDraft { session_id, draft } => {
                self.apply_save_draft(session_id, draft);
                None
            }

            _ => None,
        }
    }
}
