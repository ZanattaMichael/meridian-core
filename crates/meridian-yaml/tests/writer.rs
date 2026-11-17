//! The writer's own cases, beside the corpus `corpus.rs` checks against.

use meridian_yaml::{write, Yaml};

#[test]
fn a_raw_scalar_is_written_as_it_is_wherever_it_sits() {
    let doc = Yaml::Map(vec![
        (
            "contents".to_owned(),
            Yaml::Raw("{{ pillar['key'] | yaml_encode }}".to_owned()),
        ),
        (
            "items".to_owned(),
            Yaml::Seq(vec![Yaml::Raw("{{ a }}".to_owned()), Yaml::str("{{ a }}")]),
        ),
    ]);
    assert_eq!(
        write(&doc),
        "contents: {{ pillar['key'] | yaml_encode }}\nitems:\n  - {{ a }}\n  - '{{ a }}'\n"
    );
    assert_eq!(write(&Yaml::Raw("x".to_owned())), "x\n");
}
