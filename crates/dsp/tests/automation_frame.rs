#[test]
fn frame_resizes_macro_buffers_for_block_length_changes() {
    let mut frame = cubed_dsp::automation::AutomationFrame::with_dimensions(1, 4, 8);
    assert_eq!(frame.macro_buffer_len(), 8);

    frame.ensure_macro_dimensions(1, 4, 128);

    assert_eq!(frame.macro_buffer_len(), 128);
    assert_eq!(frame.macro_slice(0, 3).len(), 128);
    assert_eq!(frame.macro_buffers().len(), 512);

    frame.set_macro_value(0, 0, 0.5);
    assert!(frame.macro_slice(0, 0).iter().all(|&value| value == 0.5));
}
