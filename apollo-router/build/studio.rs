use std::error::Error;
use std::path::PathBuf;

pub fn main() -> Result<(), Box<dyn Error>> {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let src = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("src");
    let proto_dir = src.join("plugins").join("telemetry").join("proto");
    let reports_src = proto_dir.join("reports.proto");
    let reports_out = out_dir.join("reports.proto");

    // Process the retrieved content to:
    //  - Insert a package Report; line after the import lines (currently only one) and before the first message definition
    //  - Remove the Apollo TS extensions [(js_use_toArray)=true] and [(js_preEncoded)=true] from the file
    //  Note: Only two in use at the moment. This may fail in future if new extensions are
    //  added to the source, so be aware future self. It will manifest as a protobuf compile
    //  error.
    let mut content = std::fs::read_to_string(&reports_src)?;
    let message = "\nmessage";
    let msg_index = content.find(message).ok_or("cannot find message string")?;
    content.insert_str(msg_index, "\npackage Reports;\n");
    content = content.replace("[(js_use_toArray) = true]", "");
    content = content.replace("[(js_preEncoded) = true]", "");
    std::fs::write(&reports_out, &content)?;

    println!("cargo:rerun-if-changed={}", reports_src.to_str().unwrap());

    // Process the proto files

    tonic_build::configure()
        .field_attribute(
            "Trace.start_time",
            "#[serde(serialize_with = \"crate::plugins::telemetry::apollo_exporter::serialize_timestamp\")]",
        )
        .field_attribute(
            "Trace.end_time",
            "#[serde(serialize_with = \"crate::plugins::telemetry::apollo_exporter::serialize_timestamp\")]",
        )
        .field_attribute(
            "FetchNode.sent_time",
            "#[serde(serialize_with = \"crate::plugins::telemetry::apollo_exporter::serialize_timestamp\")]",
        )
        .field_attribute(
            "FetchNode.received_time",
            "#[serde(serialize_with = \"crate::plugins::telemetry::apollo_exporter::serialize_timestamp\")]",
        )
        .field_attribute(
            "Report.end_time",
            "#[serde(serialize_with = \"crate::plugins::telemetry::apollo_exporter::serialize_timestamp\")]",
        )
        .type_attribute(".", "#[derive(serde::Serialize)]")
        .type_attribute(".", "#[allow(dead_code)]")
        .type_attribute("StatsContext", "#[derive(Eq, Hash)]")
        .emit_rerun_if_changed(false)
        .compile_protos(&[reports_out],  &[&out_dir])?;

    Ok(())
}
