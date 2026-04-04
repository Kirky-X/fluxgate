use moka::future::Cache;

pub struct RouterCache {
    cache: Cache<String, super::RoutingDecision>,
}

impl RouterCache {
    pub fn new(max_capacity: u64, ttl: std::time::Duration) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(max_capacity)
                .time_to_live(ttl)
                .build(),
        }
    }
    
    pub async fn get(&self, key: &str) -> Option<super::RoutingDecision> {
        self.cache.get(key).await
    }
    
    pub async fn set(&self, key: String, decision: super::RoutingDecision) {
        self.cache.insert(key, decision).await;
    }
    
    pub async fn invalidate(&self) {
        self.cache.invalidate_all().await;
    }
    
    pub async fn invalidate_model(&self, model: &str) {
        let keys: Vec<String> = self.cache.iter()
            .filter(|(k, _)| k.contains(model))
            .map(|(k, _)| k.clone())
            .collect();
        
        for key in keys {
            self.cache.remove(&key).await;
        }
    }
}
