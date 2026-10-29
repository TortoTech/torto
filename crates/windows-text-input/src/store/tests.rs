use super::*;
use std::cell::Cell;

#[implement(ITextStoreACPSink)]
struct Sink {
    action: Box<dyn Fn(u32) -> Result<()>>,
}
impl ITextStoreACPSink_Impl for Sink_Impl {
    fn OnTextChange(&self, _: TEXT_STORE_TEXT_CHANGE_FLAGS, _: *const TS_TEXTCHANGE) -> Result<()> {
        Ok(())
    }
    fn OnSelectionChange(&self) -> Result<()> {
        Ok(())
    }
    fn OnLayoutChange(&self, _: TsLayoutCode, _: u32) -> Result<()> {
        Ok(())
    }
    fn OnStatusChange(&self, _: u32) -> Result<()> {
        Ok(())
    }
    fn OnAttrsChange(&self, _: i32, _: i32, _: u32, _: *const GUID) -> Result<()> {
        Ok(())
    }
    fn OnLockGranted(&self, flags: TEXT_STORE_LOCK_FLAGS) -> Result<()> {
        (self.action)(flags.0)
    }
    fn OnStartEditTransaction(&self) -> Result<()> {
        Ok(())
    }
    fn OnEndEditTransaction(&self) -> Result<()> {
        Ok(())
    }
}
fn fixture(text: &str) -> (ITextStoreACP, Rc<RefCell<Data>>) {
    let mut data = Data::new(HWND::default(), Box::new(|| {}));
    data.active = true;
    data.text = text.encode_utf16().collect();
    data.snapshot = Some(Snapshot {
        id: egui::Id::unique("test"),
        text: text.into(),
        selection: CCursorRange::default(),
        rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 200.0)),
        cursor_rect: egui::Rect::ZERO,
        multiline: true,
        char_limit: usize::MAX,
    });
    let data = Rc::new(RefCell::new(data));
    let store: ITextStoreACP = Store { data: data.clone() }.into();
    (store, data)
}
fn run(
    store: &ITextStoreACP,
    flags: u32,
    action: impl Fn(u32) -> Result<()> + 'static,
) -> Result<HRESULT> {
    let sink: ITextStoreACPSink = Sink {
        action: Box::new(action),
    }
    .into();
    unsafe {
        store.AdviseSink(
            &ITextStoreACPSink::IID,
            &sink,
            TS_AS_TEXT_CHANGE | TS_AS_SEL_CHANGE,
        )?;
        let result = store.RequestLock(flags);
        store.UnadviseSink(&sink)?;
        result
    }
}

#[test]
fn tsf_reads_complete_multiline_text_in_chunks_even_across_surrogate_pairs() -> Result<()> {
    let text = "first🙂\n第二行".repeat(1200);
    let (store, _) = fixture(&text);
    let locked = store.clone();
    let expected = text.encode_utf16().collect::<Vec<_>>();
    let result = run(&store, TS_LF_READ.0 | TS_LF_SYNC, move |_| {
        unsafe {
            assert_eq!(locked.GetEndACP()? as usize, expected.len());
            let mut actual = Vec::new();
            let mut start = 0;
            loop {
                let mut buffer = [0u16; 7];
                let mut written = 0;
                let mut runs = 0;
                let mut next = 0;
                locked.GetText(
                    start,
                    -1,
                    &mut buffer,
                    &mut written,
                    &mut [],
                    &mut runs,
                    &mut next,
                )?;
                actual.extend_from_slice(&buffer[..written as usize]);
                if written == 0 {
                    break;
                }
                assert!(next > start);
                start = next;
            }
            assert_eq!(actual, expected);
        }
        Ok(())
    })?;
    assert!(result.is_ok());
    Ok(())
}

#[test]
fn tsf_candidate_geometry_keeps_visible_composition_and_caret_anchors_unclipped() -> Result<()> {
    let (_, data) = fixture("alpha ni");
    let mut data = data.borrow_mut();
    data.start = 8;
    data.end = 8;
    data.composition = Some(6..8);
    let caret = egui::Rect::from_min_size(egui::pos2(100.0, 60.0), egui::vec2(1.0, 20.0));
    data.snapshot.as_mut().unwrap().cursor_rect = caret;
    assert_eq!(text_bounds(&data, 6, 8)?, (caret, false));
    assert_eq!(text_bounds(&data, 8, 8)?, (caret, false));
    assert_eq!(
        text_bounds(&data, 0, 5)?,
        (data.snapshot.as_ref().unwrap().rect, false)
    );
    data.snapshot.as_mut().unwrap().cursor_rect = caret.translate(egui::vec2(399.5, 0.0));
    let (visible, clipped) = text_bounds(&data, 6, 8)?;
    assert!(
        !clipped,
        "caret padding at the editor edge does not hide the composed text"
    );
    assert_eq!(visible.right(), 500.0);
    assert!(visible.width() > 0.0);
    data.snapshot.as_mut().unwrap().cursor_rect = caret.translate(egui::vec2(500.0, 0.0));
    assert_eq!(text_bounds(&data, 6, 8)?, (egui::Rect::ZERO, true));
    Ok(())
}

#[test]
fn tsf_enforces_read_write_locks_and_rejects_invalid_utf16_edits() -> Result<()> {
    let (store, data) = fixture("a🙂b");
    assert_eq!(
        unsafe { store.GetEndACP() }.unwrap_err().code(),
        TS_E_NOLOCK
    );
    let locked = store.clone();
    assert!(
        run(&store, TS_LF_READ.0, move |_| {
            let value: Vec<u16> = "new".encode_utf16().collect();
            assert_eq!(
                unsafe { locked.SetText(0, 0, 1, &value) }
                    .unwrap_err()
                    .code(),
                TS_E_NOLOCK
            );
            Ok(())
        })?
        .is_ok()
    );
    let locked = store.clone();
    assert!(
        run(&store, TS_LF_READWRITE.0, move |_| {
            let value: Vec<u16> = "new".encode_utf16().collect();
            assert_eq!(
                unsafe { locked.SetText(0, 2, 3, &value) }
                    .unwrap_err()
                    .code(),
                TS_E_INVALIDPOS
            );
            assert!(unsafe { locked.SetText(0, 0, 1, &[0xD800]) }.is_err());
            Ok(())
        })?
        .is_ok()
    );
    assert_eq!(String::from_utf16(&data.borrow().text).unwrap(), "a🙂b");
    Ok(())
}

#[test]
fn tsf_replacement_and_reverse_selection_preserve_the_full_document() -> Result<()> {
    let (store, data) = fixture("old\n中文🙂");
    let locked = store.clone();
    assert!(
        run(&store, TS_LF_READWRITE.0, move |_| {
            unsafe {
                let text: Vec<u16> = "translated\n新🙂".encode_utf16().collect();
                let length = locked.GetEndACP()?;
                let change = locked.SetText(0, 0, length, &text)?;
                assert_eq!(change.acpOldEnd, length);
                assert_eq!(change.acpNewEnd, text.len() as i32);
                locked.SetSelection(&[TS_SELECTION_ACP {
                    acpStart: 2,
                    acpEnd: 5,
                    style: TS_SELECTIONSTYLE {
                        ase: TS_AE_START,
                        fInterimChar: false.into(),
                    },
                }])?;
            }
            Ok(())
        })?
        .is_ok()
    );
    let d = data.borrow();
    assert_eq!(String::from_utf16(&d.text).unwrap(), "translated\n新🙂");
    assert_eq!(selection(&d).primary.index, 2.into());
    assert_eq!(selection(&d).secondary.index, 5.into());
    assert!(d.dirty);
    Ok(())
}

#[test]
fn tsf_lock_upgrade_is_deferred_without_reentrant_borrows() -> Result<()> {
    let (store, _) = fixture("text");
    let locked = store.clone();
    let calls = Rc::new(Cell::new(0));
    let count = calls.clone();
    assert!(
        run(&store, TS_LF_READ.0, move |flags| {
            count.set(count.get() + 1);
            if count.get() == 1 {
                assert_eq!(
                    unsafe { locked.RequestLock(TS_LF_READWRITE.0 | TS_LF_SYNC) }?,
                    TS_E_SYNCHRONOUS
                );
                assert_eq!(
                    unsafe { locked.RequestLock(TS_LF_READWRITE.0) }?,
                    TS_S_ASYNC
                );
            } else {
                assert_eq!(flags, TS_LF_READWRITE.0);
            }
            Ok(())
        })?
        .is_ok()
    );
    assert_eq!(calls.get(), 2);
    Ok(())
}

#[test]
fn tsf_insert_query_does_not_modify_text_and_limits_are_enforced() -> Result<()> {
    let (store, data) = fixture("original");
    data.borrow_mut().snapshot.as_mut().unwrap().char_limit = 8;
    let locked = store.clone();
    assert!(
        run(&store, TS_LF_READWRITE.0, move |_| {
            unsafe {
                let value: Vec<u16> = "more".encode_utf16().collect();
                let mut start = 0;
                let mut end = 0;
                locked.InsertTextAtSelection(
                    TS_IAS_QUERYONLY,
                    &value,
                    &mut start,
                    &mut end,
                    std::ptr::null_mut(),
                )?;
                assert!(
                    locked
                        .InsertTextAtSelection(
                            TS_IAS_NOQUERY,
                            &value,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            &mut TS_TEXTCHANGE::default()
                        )
                        .is_err()
                );
                assert!(
                    locked
                        .SetSelection(&[TS_SELECTION_ACP {
                            acpStart: -1,
                            acpEnd: 2,
                            style: TS_SELECTIONSTYLE::default()
                        }])
                        .is_err()
                );
            }
            Ok(())
        })?
        .is_ok()
    );
    assert_eq!(String::from_utf16(&data.borrow().text).unwrap(), "original");
    Ok(())
}
