use std::{sync::{atomic::{AtomicU32, Ordering}, Arc}, time::Duration, collections::HashMap};

use rdkafka::{ClientConfig, consumer::{StreamConsumer, Consumer}, Message};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_postgres::{NoTls, types::ToSql};

#[derive(Debug)]
struct KafkaMessage {
    message_type: String,
    key: String,
    payload: tokio_postgres::types::Json<Value>,
}

fn create_batch_query(n: usize) -> String {
    let columns = vec!["variation_id", "data"];

    let mut values = Vec::with_capacity(n);

    for i in 0..n {
        let placeholders: String = (0..columns.len())
            .map(|j| format!("${}", (i * columns.len()) + j + 1))
            .collect::<Vec<_>>()
            .join(",");

        values.push(format!("({})", placeholders));
    }

    let conflicts: Vec<String> = columns.iter()
        .map(|col| format!("{} = excluded.{}", col, col))
        .collect();

    return format!("
    insert into variations ({})
    values {}
    on conflict (variation_id)
        do update set {}
    ", columns.join(","), values.join(","), conflicts.join(","));
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let kafka_brokers = std::env::var("KAFKA_BROKERS")?;
    let kafka_topic = std::env::var("KAFKA_TOPIC")?;
    let kafka_group_id = std::env::var("KAFKA_GROUP_ID")?;
    let kafka_username = std::env::var("KAFKA_USERNAME")?;
    let kafka_password = std::env::var("KAFKA_PASSWORD")?;
    let postgres_host = std::env::var("POSTGRES_HOST")?;
    let postgres_port = std::env::var("POSTGRES_PORT")?;
    let postgres_username = std::env::var("POSTGRES_USERNAME")?;
    let postgres_password = std::env::var("POSTGRES_PASSWORD")?;
    let postgres_database = std::env::var("POSTGRES_DATABASE")?;

    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", kafka_brokers)
		.set("group.id", kafka_group_id)
		.set("sasl.username", kafka_username)
		.set("sasl.password", kafka_password)
		.set("security.protocol", "sasl_ssl")
		.set("sasl.mechanisms", "PLAIN")
		.set("auto.offset.reset", "earliest")
		.set("statistics.interval.ms", "10000")
		.set("enable.auto.commit", "false")
        .create()
        .expect("kafka create error");

    consumer
        .subscribe(&[&kafka_topic])
        .expect("could not subscribe to topic");

    let dsn = format!(
        "postgres://{}:{}@{}:{}/{}",
        postgres_username,
        postgres_password,
        postgres_host,
        postgres_port,
        postgres_database,
    );

    let (client, connection) =
        tokio_postgres::connect(&dsn, NoTls).await?;

    tokio::spawn(async {
        if let Err(e) = connection.await {
            eprintln!("postgres error: {}", e);
        }
    });

    client.execute(r#"
        create table if not exists variations (
            variation_id text primary key,
            data jsonb
        )
    "#, &[]).await?;

    let (tx, mut rx) = mpsc::channel(1000);
    let num_messages = Arc::new(AtomicU32::new(0));

    {
        let num_messages = num_messages.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                println!("{:?} messages/sec", num_messages);
                num_messages.store(0, Ordering::SeqCst);
            }
        });
    }

    tokio::spawn(async move {
        let mut set = HashMap::new();

        loop {
            let message: KafkaMessage = rx.recv()
                .await
                .unwrap();

            set.insert(message.key, message.payload);
            num_messages.fetch_add(1, Ordering::SeqCst);

            if set.len() < 1000 {
                continue;
            }

            let params: Vec<&(dyn ToSql + Sync)> = set.iter()
                .flat_map(|(key, payload)| {
                    let variation_id: &(dyn ToSql + Sync) = key;
                    let data: &(dyn ToSql + Sync) = payload;
                    vec![variation_id, data]
                })
                .collect();


            let query = create_batch_query(set.len());

            client
                .execute(&query, params.as_slice())
                .await
                .expect("could not upsert variation");

            set.clear();
        }
    });

    loop {
        let message = consumer.recv().await?;

        if message.payload_len() > 0 {
            let key = message
                .key()
                .expect("could not load key");

            let payload = message
                .payload()
                .expect("could not read payload");

            let json_value: Value = serde_json::from_slice(payload).unwrap();

            tx.send(KafkaMessage {
                key: String::from_utf8(key.into()).unwrap(),
                message_type: String::from("update"),
                payload: tokio_postgres::types::Json(json_value),
            }).await.unwrap();
        }
    }
}
