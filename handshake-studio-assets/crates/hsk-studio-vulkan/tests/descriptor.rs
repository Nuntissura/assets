use hsk_studio_vulkan::DESCRIPTOR;

#[test]
fn descriptor_names_this_crate() {
    assert!(DESCRIPTOR.starts_with('{') && DESCRIPTOR.ends_with('}'));
    assert!(DESCRIPTOR.contains(r#""crate":"hsk-studio-vulkan""#));
}
