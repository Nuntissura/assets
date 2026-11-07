use hsk_studio_native_host::DESCRIPTOR;

#[test]
fn descriptor_names_this_crate() {
    assert!(DESCRIPTOR.starts_with('{') && DESCRIPTOR.ends_with('}'));
    assert!(DESCRIPTOR.contains(r#""crate":"hsk-studio-native-host""#));
}
