#![doc = include_str!("../README.md")]
#![doc(test(attr(deny(warnings))))]
#![doc(test(attr(allow(deprecated))))]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![doc(html_favicon_url = "https://raw.githubusercontent.com/oxigraph/oxigraph/main/logo.svg")]
#![doc(html_logo_url = "https://raw.githubusercontent.com/oxigraph/oxigraph/main/logo.svg")]

pub mod io;
pub mod model;
pub mod sparql;
mod storage;
pub mod store;

// This is the interface to the JVM that we'll call the majority of our
// methods on.
use jni::JNIEnv;

// These objects are what you should use as arguments to your native
// function. They carry extra lifetime information to prevent them escaping
// this context and getting used after being GC'd.
use jni::objects::{JClass, JString};

// This is just a pointer. We'll be returning it from our function. We
// can't return one of the objects with lifetime information because the
// lifetime checker won't let us.
use jni::sys::jstring;

use preference_analyzer::preference_extractor::PreferenceExtractor;

use crate::io::RdfFormat;
use crate::sparql::results::QueryResultsFormat;
use crate::store::Store;

// Test DATA Values
// #[allow(clippy::non_ascii_literal)]
// const DATA: &str = r#"
// @prefix wd: <http://www.wikidata.org/entity/> .

// wd:FamilyDinner wd:isOn "2024-08-03 18:00 - 20:00" .
// wd:abc wd:efg "111111111" .

// "#;

#[no_mangle]
pub extern "system" fn Java_ai_mlc_mlcchat_MainActivity_loadData<'local>(
    mut env: JNIEnv<'local>,
    // This is the class that owns our static method. It's not going to be used,
    // but still must be present to match the expected signature of a static
    // native method.
    _class: JClass<'local>,
    line_num: usize,
    data: JString<'local>,
    input: JString<'local>,
) -> jstring {
    let store = Store::new().unwrap();
    // let _unused = store.load_from_read(RdfFormat::Turtle, DATA.as_bytes());
    // let _ = store.validate();

    // First, we have to get the string out of Java. Check out the `strings`
    // module for more info on how this works.
    // let event_message: String =
    //     env.get_string(&input).expect("Couldn't get java string!").into();

    // let mut results = "Answer:".to_owned();
    // let triples = store.query("SELECT * WHERE {{ ?s ?p ?o }}");
    // results.push_str(std::str::from_utf8(
    //     &triples.expect("ALL").write(Vec::new(), QueryResultsFormat::Json).expect("VEC")).expect("UTF")
    // );
    let personal_data: String = env
        .get_string(&data)
        .expect("Couldn't get the Java string of the given data!")
        .into();
    let input_message: String = env
        .get_string(&input)
        .expect("Couldn't get the Java string of the given input.!")
        .into();
    let (results, count) =
        PreferenceExtractor::extract_preference_ondevice(line_num, personal_data, input_message);

    let ret = env
        .new_string(format!("{}", results))
        .expect("Couldn't create java string!");
    ret.into_raw()
}
