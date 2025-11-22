use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use tokio::time::{sleep, Duration};

/// Configuration for a single service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    /// Command to run the service
    pub command: String,
    
    /// Port the service runs on
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    
    /// Working directory for the service
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    
    /// Environment variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    
    /// Services this depends on (must start first)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    
    /// Health check command (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<String>,
    
    /// Delay before considering service started (seconds)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_delay: Option<u64>,
}

/// Main orchestration configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationConfig {
    /// Version of the config format
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    
    /// Services to manage
    pub services: HashMap<String, ServiceConfig>,
    
    /// Global environment variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// Tracks running services
#[derive(Debug)]
pub struct RunningService {
    pub name: String,
    pub pid: u32,
    pub port: Option<u16>,
    pub child: Option<Child>,
}

/// Service orchestrator
pub struct Orchestrator {
    config: OrchestrationConfig,
    config_path: PathBuf,
    running_services: HashMap<String, RunningService>,
}

impl Orchestrator {
    /// Load configuration from a file
    pub fn load(config_path: &Path) -> Result<Self> {
        let content = fs::read_to_string(config_path)
            .context(format!("Failed to read config file: {}", config_path.display()))?;
        
        let config: OrchestrationConfig = serde_yaml::from_str(&content)
            .context("Failed to parse YAML configuration")?;
        
        Ok(Self {
            config,
            config_path: config_path.to_path_buf(),
            running_services: HashMap::new(),
        })
    }
    
    /// Try to find and load config from default locations
    pub fn load_from_default() -> Result<Self> {
        let possible_paths = vec![
            PathBuf::from(".port-kill.yaml"),
            PathBuf::from(".port-kill.yml"),
            PathBuf::from("port-kill.yaml"),
            PathBuf::from("port-kill.yml"),
        ];
        
        for path in possible_paths {
            if path.exists() {
                return Self::load(&path);
            }
        }
        
        Err(anyhow::anyhow!(
            "No configuration file found. Create .port-kill.yaml in your project root."
        ))
    }
    
    /// Get the configuration
    pub fn config(&self) -> &OrchestrationConfig {
        &self.config
    }
    
    /// Start all services in dependency order
    pub async fn start_all(&mut self) -> Result<()> {
        log::info!("Starting all services...");
        
        let service_order = self.resolve_dependencies()?;
        
        for service_name in service_order {
            self.start_service(&service_name).await?;
        }
        
        log::info!("All services started successfully");
        Ok(())
    }
    
    /// Stop all running services
    pub async fn stop_all(&mut self) -> Result<()> {
        log::info!("Stopping all services...");
        
        let service_names: Vec<String> = self.running_services.keys().cloned().collect();
        
        for service_name in service_names.iter().rev() {
            self.stop_service(service_name).await?;
        }
        
        self.running_services.clear();
        log::info!("All services stopped");
        Ok(())
    }
    
    /// Start a specific service
    pub async fn start_service(&mut self, service_name: &str) -> Result<()> {
        if self.running_services.contains_key(service_name) {
            log::warn!("Service '{}' is already running", service_name);
            return Ok(());
        }
        
        let service_config = self.config.services.get(service_name)
            .ok_or_else(|| anyhow::anyhow!("Service '{}' not found in configuration", service_name))?
            .clone();
        
        log::info!("Starting service '{}'...", service_name);
        
        // Start dependencies first
        if let Some(ref deps) = service_config.depends_on {
            for dep in deps {
                if !self.running_services.contains_key(dep) {
                    // Box the recursive call to avoid infinite size
                    Box::pin(self.start_service(dep)).await?;
                }
            }
        }
        
        // Parse and execute the command
        let parts = Self::parse_command(&service_config.command);
        if parts.is_empty() {
            return Err(anyhow::anyhow!("Empty command for service '{}'", service_name));
        }
        
        let program = &parts[0];
        let args = &parts[1..];
        
        let working_dir = if let Some(ref dir) = service_config.dir {
            PathBuf::from(dir)
        } else {
            self.config_path.parent().unwrap_or(Path::new(".")).to_path_buf()
        };
        
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(&working_dir);
        
        // Add environment variables
        if let Some(ref global_env) = self.config.env {
            cmd.envs(global_env);
        }
        if let Some(ref service_env) = service_config.env {
            cmd.envs(service_env);
        }
        
        // Spawn the process
        let child = cmd.spawn()
            .context(format!("Failed to start service '{}': {}", service_name, service_config.command))?;
        
        let pid = child.id();
        
        log::info!("Service '{}' started with PID {}", service_name, pid);
        
        // Wait for startup delay if specified
        if let Some(delay) = service_config.startup_delay {
            log::info!("Waiting {} seconds for service '{}' to start...", delay, service_name);
            sleep(Duration::from_secs(delay)).await;
        }
        
        // Store running service
        self.running_services.insert(
            service_name.to_string(),
            RunningService {
                name: service_name.to_string(),
                pid,
                port: service_config.port,
                child: Some(child),
            },
        );
        
        Ok(())
    }
    
    /// Stop a specific service
    pub async fn stop_service(&mut self, service_name: &str) -> Result<()> {
        let mut service = match self.running_services.remove(service_name) {
            Some(s) => s,
            None => {
                log::warn!("Service '{}' is not running", service_name);
                return Ok(());
            }
        };
        
        log::info!("Stopping service '{}' (PID {})...", service_name, service.pid);
        
        if let Some(mut child) = service.child.take() {
            // Try graceful shutdown first
            #[cfg(not(target_os = "windows"))]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                
                if let Err(e) = kill(Pid::from_raw(service.pid as i32), Signal::SIGTERM) {
                    log::warn!("Failed to send SIGTERM to service '{}': {}", service_name, e);
                }
                
                // Wait a bit for graceful shutdown
                sleep(Duration::from_millis(500)).await;
            }
            
            #[cfg(target_os = "windows")]
            {
                let _ = Command::new("taskkill")
                    .args(&["/PID", &service.pid.to_string(), "/T"])
                    .output();
                
                sleep(Duration::from_millis(500)).await;
            }
            
            // Force kill if still running
            match child.kill() {
                Ok(_) => log::info!("Service '{}' stopped", service_name),
                Err(e) => log::warn!("Failed to kill service '{}': {}", service_name, e),
            }
        }
        
        Ok(())
    }
    
    /// Restart a specific service
    pub async fn restart_service(&mut self, service_name: &str) -> Result<()> {
        log::info!("Restarting service '{}'...", service_name);
        
        self.stop_service(service_name).await?;
        sleep(Duration::from_secs(1)).await;
        self.start_service(service_name).await?;
        
        log::info!("Service '{}' restarted successfully", service_name);
        Ok(())
    }
    
    /// Get status of all services
    pub fn get_status(&self) -> Vec<ServiceStatus> {
        let mut statuses = Vec::new();
        
        for (name, config) in &self.config.services {
            let running = self.running_services.get(name);
            
            statuses.push(ServiceStatus {
                name: name.clone(),
                running: running.is_some(),
                pid: running.map(|s| s.pid),
                port: config.port,
                command: config.command.clone(),
            });
        }
        
        statuses.sort_by(|a, b| a.name.cmp(&b.name));
        statuses
    }
    
    // Private helper methods
    
    fn resolve_dependencies(&self) -> Result<Vec<String>> {
        let mut visited = std::collections::HashSet::new();
        let mut order = Vec::new();
        
        for service_name in self.config.services.keys() {
            self.visit_service(service_name, &mut visited, &mut order)?;
        }
        
        Ok(order)
    }
    
    fn visit_service(
        &self,
        service_name: &str,
        visited: &mut std::collections::HashSet<String>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        if visited.contains(service_name) {
            return Ok(());
        }
        
        let service = self.config.services.get(service_name)
            .ok_or_else(|| anyhow::anyhow!("Service '{}' not found", service_name))?;
        
        if let Some(ref deps) = service.depends_on {
            for dep in deps {
                self.visit_service(dep, visited, order)?;
            }
        }
        
        visited.insert(service_name.to_string());
        order.push(service_name.to_string());
        
        Ok(())
    }
    
    fn parse_command(command: &str) -> Vec<String> {
        let mut parts = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut chars = command.chars().peekable();
        
        while let Some(c) = chars.next() {
            match c {
                '"' | '\'' => {
                    in_quotes = !in_quotes;
                }
                ' ' | '\t' if !in_quotes => {
                    if !current.is_empty() {
                        parts.push(current.clone());
                        current.clear();
                    }
                }
                _ => {
                    current.push(c);
                }
            }
        }
        
        if !current.is_empty() {
            parts.push(current);
        }
        
        parts
    }
}

/// Status of a service
#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub name: String,
    pub running: bool,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub command: String,
}

/// Create a sample configuration file
pub fn create_sample_config(path: &Path) -> Result<()> {
    let sample = r#"# Port Kill Orchestration Configuration
# Documentation: https://github.com/treadiehq/port-kill

version: "1"

# Global environment variables (applied to all services)
env:
  NODE_ENV: development
  DEBUG: "true"

services:
  # Frontend service
  frontend:
    command: npm run dev
    port: 3000
    dir: ./frontend
    startup_delay: 2
    env:
      PORT: "3000"
  
  # Backend API service
  backend:
    command: npm run start
    port: 8000
    dir: ./backend
    depends_on:
      - database
    env:
      PORT: "8000"
      DATABASE_URL: postgres://localhost:5432/myapp
  
  # Database service
  database:
    command: docker-compose up database
    port: 5432
    startup_delay: 5
"#;
    
    fs::write(path, sample)
        .context("Failed to write sample configuration")?;
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_command() {
        let cmd = "npm run dev --port 3000";
        let parts = Orchestrator::parse_command(cmd);
        assert_eq!(parts, vec!["npm", "run", "dev", "--port", "3000"]);
    }
    
    #[test]
    fn test_parse_command_with_quotes() {
        let cmd = r#"node "my script.js" --arg "value with spaces""#;
        let parts = Orchestrator::parse_command(cmd);
        assert_eq!(parts, vec!["node", "my script.js", "--arg", "value with spaces"]);
    }
}

