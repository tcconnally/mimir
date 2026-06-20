// Bundled embedding model — zero-dependency dense embeddings using ONNX Runtime.
//
// When --embedding-model is set, uses a local ONNX model for embeddings
// instead of requiring Ollama. The all-MiniLM-L6-v2 model (80MB, 384-dim) is
// downloaded on first use and cached at ~/.mimir/models/.
//
// Inference backends:
//   - Native (feature = "bundled-embeddings"): ort + tokenizers crates
//   - Fallback: uses onnxruntime via Python subprocess (requires `pip install onnxruntime`)
//   - When neither is available, falls through to Ollama (if configured)

use std::path::PathBuf;

/// Configuration for the local embedding backend.
#[derive(Clone)]
pub struct EmbeddingConfig {
    /// Whether local embeddings are enabled.
    #[allow(dead_code)]
    pub enabled: bool,
    /// Path to the ONNX model file.
    #[allow(dead_code)]
    pub model_path: PathBuf,
}

impl EmbeddingConfig {
    #[allow(dead_code)]
    pub fn with_model_path(path: PathBuf) -> Self {
        EmbeddingConfig {
            enabled: true,
            model_path: path,
        }
    }

    /// Default model path in ~/.mimir/models/
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home)
            .join(".mimir")
            .join("models")
            .join("all-MiniLM-L6-v2")
            .join("model.onnx")
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        EmbeddingConfig {
            enabled: false,
            model_path: Self::default_path(),
        }
    }
}

// ─── Model Download ─────────────────────────────────────────────────────

/// Download the all-MiniLM-L6-v2 ONNX model from HuggingFace if not already cached.
#[allow(dead_code)]
pub fn ensure_model(config: &EmbeddingConfig) -> Result<(), String> {
    let model_dir = config
        .model_path
        .parent()
        .ok_or_else(|| "invalid model path".to_string())?;

    std::fs::create_dir_all(model_dir)
        .map_err(|e| format!("failed to create model directory: {}", e))?;

    if !config.model_path.exists() {
        eprintln!(
            "mimir: downloading embedding model to {} ...",
            config.model_path.display()
        );
        download_file(
            "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx",
            &config.model_path,
        )?;
    }

    let tokenizer_path = model_dir.join("tokenizer.json");
    if !tokenizer_path.exists() {
        eprintln!(
            "mimir: downloading tokenizer to {} ...",
            tokenizer_path.display()
        );
        download_file(
            "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json",
            &tokenizer_path,
        )?;
    }

    Ok(())
}

#[allow(dead_code)]
fn download_file(url: &str, dest: &PathBuf) -> Result<(), String> {
    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(600))
        .call()
        .map_err(|e| format!("download failed for {}: {}", url, e))?;

    let total = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok());

    let mut reader = response.into_reader();
    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("failed to create file: {}", e))?;

    let mut buf = [0u8; 65536];
    let mut downloaded: u64 = 0;
    loop {
        let n =
            std::io::Read::read(&mut reader, &mut buf).map_err(|e| format!("read error: {}", e))?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n])
            .map_err(|e| format!("write error: {}", e))?;
        downloaded += n as u64;
        if let Some(total) = total {
            if downloaded % (1024 * 1024) < 65536 || downloaded == total {
                eprint!(
                    "\r  {:.1}% ({:.1} MB / {:.1} MB)",
                    (downloaded as f64 / total as f64) * 100.0,
                    downloaded as f64 / (1024.0 * 1024.0),
                    total as f64 / (1024.0 * 1024.0)
                );
            }
        }
    }
    if total.is_some() {
        eprintln!();
    }
    Ok(())
}

// ─── Embedding Generation ───────────────────────────────────────────────

/// Generate a 384-dimensional embedding vector for the given text.
///
/// Tries backends in order:
///   1. Native ort+tokenizers (if feature "bundled-embeddings" was enabled at build time)
///   2. Python onnxruntime (if `python3` is on PATH and `onnxruntime` is installed)
///   3. Returns an error suggesting either option
#[allow(dead_code)]
pub fn generate_embedding(
    config: &EmbeddingConfig,
    text: &str,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    ensure_model(config).map_err(|e| format!("model setup failed: {}", e))?;

    // Try native ort backend first
    #[cfg(feature = "bundled-embeddings")]
    {
        match generate_with_ort(config, text) {
            Ok(vec) => return Ok(vec),
            Err(e) => eprintln!(
                "mimir: native embedding failed ({}), trying Python fallback...",
                e
            ),
        }
    }

    // Try Python onnxruntime fallback
    match generate_with_python(config, text) {
        Ok(vec) => return Ok(vec),
        Err(e) => eprintln!("mimir: Python embedding fallback failed ({})", e),
    }

    Err("No embedding backend available. Options:\n\
         - Rebuild with: cargo build --release --features bundled-embeddings\n\
         - Install Python + onnxruntime: pip install onnxruntime\n\
         - Use Ollama: mimir serve --llm-endpoint http://localhost:11434"
        .into())
}

// ─── Native ort backend (feature = "bundled-embeddings") ─────────────────

#[cfg(feature = "bundled-embeddings")]
fn generate_with_ort(
    config: &EmbeddingConfig,
    text: &str,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    use ort::session::Session;

    let model_dir = config
        .model_path
        .parent()
        .expect("model_path must have a parent directory");
    let tokenizer_path = model_dir.join("tokenizer.json");

    let session = Session::builder()?.commit_from_file(&config.model_path)?;

    let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| format!("failed to load tokenizer: {}", e))?;

    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| format!("tokenization failed: {}", e))?;

    let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let attention_mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| m as i64)
        .collect();

    let shape = [token_ids.len()];
    // Use ndarray for tensor creation (ort 2.x uses ndarray)
    let input_array = ndarray::Array1::from_vec(token_ids.clone());
    let mask_array = ndarray::Array1::from_vec(attention_mask.clone());

    let input_tensor =
        ort::value::TensorRef::from_array_view(&input_array.insert_axis(ndarray::Axis(0)))?;
    let mask_tensor =
        ort::value::TensorRef::from_array_view(&mask_array.insert_axis(ndarray::Axis(0)))?;

    let outputs = session.run(ort::inputs![
        "input_ids" => input_tensor,
        "attention_mask" => mask_tensor,
    ]?)?;

    // Extract last_hidden_state and mean pool
    let hidden: &ort::value::TensorRef<f32> = outputs["last_hidden_state"].extract_tensor()?;
    let view = hidden.view();
    let seq_len = view.shape()[1];
    let dim = view.shape()[2];

    let mut pooled = vec![0.0f32; dim];
    let mut active = 0usize;
    for t in 0..seq_len {
        if t < attention_mask.len() && attention_mask[t] == 1 {
            for d in 0..dim {
                pooled[d] += view[[0, t, d]];
            }
            active += 1;
        }
    }
    if active > 0 {
        let n = active as f32;
        for v in pooled.iter_mut() {
            *v /= n;
        }
    }

    // L2 normalize
    let norm: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in pooled.iter_mut() {
            *v /= norm;
        }
    }

    Ok(pooled)
}

// ─── Python onnxruntime fallback ─────────────────────────────────────────

/// Generate embeddings using a Python helper that calls onnxruntime.
#[allow(dead_code)]
fn generate_with_python(
    config: &EmbeddingConfig,
    text: &str,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let model_dir = config
        .model_path
        .parent()
        .ok_or_else(|| "invalid model path".to_string())?;
    let tokenizer_path = model_dir.join("tokenizer.json");

    let model_str = config.model_path.to_string_lossy();
    let tokenizer_str = tokenizer_path.to_string_lossy();
    let text_escaped = text.replace('\\', "\\\\").replace('\'', "\\'");

    let script = format!(
        r#"
import sys, json, numpy as np
try:
    import onnxruntime as ort
except ImportError:
    print(json.dumps({{"error": "onnxruntime not installed. Run: pip install onnxruntime"}}))
    sys.exit(1)

try:
    from tokenizers import Tokenizer
    tokenizer = Tokenizer.from_file('{}')
    encoding = tokenizer.encode('{}')
    input_ids = np.array([encoding.ids], dtype=np.int64)
    attention_mask = np.array([encoding.attention_mask], dtype=np.int64)

    session = ort.InferenceSession('{}')
    outputs = session.run(None, {{
        'input_ids': input_ids,
        'attention_mask': attention_mask,
    }})
    hidden = outputs[0]  # [1, seq_len, 384]

    # Mean pooling with attention mask
    mask = attention_mask[0, :, None]  # [seq_len, 1]
    pooled = (hidden[0] * mask).sum(axis=0) / mask.sum()
    # L2 normalize
    norm = np.linalg.norm(pooled)
    if norm > 0:
        pooled = pooled / norm

    print(json.dumps({{"embedding": pooled.tolist()}}))
except Exception as e:
    print(json.dumps({{"error": str(e)}}))
    sys.exit(1)
"#,
        tokenizer_str, text_escaped, model_str
    );

    let output = std::process::Command::new("python3")
        .arg("-c")
        .arg(&script)
        .output()
        .map_err(|e| {
            format!(
                "failed to run python3: {}. Install Python 3 and onnxruntime.",
                e
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("python3 embedding failed: {}", stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("failed to parse python output: {} — raw: {}", e, stdout))?;

    if let Some(err) = result.get("error") {
        return Err(format!("python embedding error: {}", err).into());
    }

    let embedding: Vec<f32> = result["embedding"]
        .as_array()
        .ok_or("missing embedding in python output")?
        .iter()
        .map(|v| v.as_f64().unwrap_or(0.0) as f32)
        .collect();

    Ok(embedding)
}
