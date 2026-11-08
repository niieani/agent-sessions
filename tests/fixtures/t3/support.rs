use rusqlite::{Connection, params};
use serde_json::json;
use std::path::Path;

pub fn fixture(base: &Path) -> Connection {
    let userdata = base.join("userdata");
    std::fs::create_dir_all(userdata.join("attachments")).unwrap();
    let c = Connection::open(userdata.join("statev2.sqlite")).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    c.execute_batch(include_str!("v2-schema.sql")).unwrap();
    c.execute_batch("ALTER TABLE projection_projects ADD COLUMN future BLOB; ALTER TABLE orchestration_v2_projection_messages ADD COLUMN future_blob BLOB;").unwrap();
    let old = "2026-08-28T10:00:00Z";
    let new = "2026-08-28T10:01:00Z";
    let thread = "11111111-1111-4111-8111-111111111111";
    c.execute("INSERT INTO projection_projects(project_id,title,workspace_root,scripts_json,created_at,updated_at,future) VALUES ('project','Demo','/work','[]',?1,?2,?3)",params![old,new,&[0xff_u8,1]]).unwrap();
    for (id, title, archived) in [
        (thread, "Live", None),
        (
            "22222222-2222-4222-8222-222222222222",
            "Archived",
            Some(new),
        ),
    ] {
        c.execute("INSERT INTO orchestration_v2_projection_threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,archived_at,deleted_at,payload_json) VALUES (?1,'project',?2,'claudeAgent','full-access','default',?3,?4,?5,?5,?6)", params![id,title,old,new,archived,json!({"modelSelection":{"instanceId":"claudeAgent","model":"claude-fable-5","options":[{"id":"effort","value":"high"}]}}).to_string()]).unwrap();
    }
    c.execute("INSERT INTO orchestration_events(sequence,event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,actor_kind,payload_json,metadata_json) VALUES (1,'pe','project','project',1,'project.updated',?1,'user','{}','{}')",[old]).unwrap();
    for i in 1..=2 {
        c.execute("INSERT INTO orchestration_v2_events(sequence,event_id,thread_id,event_type,occurred_at,payload_json) VALUES (?1,?2,?3,'future',?4,'{}')",params![i,format!("e{i}"),thread,new]).unwrap();
    }
    c.execute("INSERT INTO orchestration_v2_projection_provider_threads(provider_thread_id,thread_id,provider,status,updated_at,payload_json) VALUES ('pt',?1,'claudeAgent','idle',?2,?3)",params![thread,new,json!({"nativeThreadRef":{"driver":"claudeAgent","nativeId":"33333333-3333-4333-8333-333333333333","strength":"strong"}}).to_string()]).unwrap();
    let attachments = json!([{"type":"file","id":"safe-attachment","name":"report.txt","mimeType":"text/plain","sizeBytes":5}]);
    c.execute("INSERT INTO orchestration_v2_projection_messages(message_id,thread_id,role,streaming,created_at,updated_at,payload_json,future_blob) VALUES ('message',?1,'user',0,?2,?2,?3,?4)",params![thread,new,json!({"text":"hello","attachments":attachments}).to_string(),&[0xff_u8,0]]).unwrap();
    c.execute("INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,completed_at,payload_json) VALUES ('run',?1,1,'claudeAgent','completed',?2,?2,'{}')",params![thread,new]).unwrap();
    c.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES ('activity',?1,1,'future','completed',?2,'{\"type\":\"future\",\"unknown\":1}')",params![thread,new]).unwrap();
    c.execute("INSERT INTO orchestration_v2_projection_checkpoints(checkpoint_id,thread_id,scope_id,node_id,ordinal_within_scope,status,captured_at,payload_json) VALUES ('checkpoint',?1,'scope','node',1,'ready',?2,'{}')", params![thread,new]).unwrap();
    std::fs::write(userdata.join("attachments/safe-attachment.txt"), b"hello").unwrap();
    c
}
