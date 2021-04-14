use std::collections::hash_map::Keys;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub struct TripleHashMap<KM, K1, K2, V>
where
    KM: Hash + Eq + Clone,
    K1: Hash + Eq + Clone,
    K2: Hash + Eq + Clone,
{
    map: HashMap<KM, (Option<(K1, K2)>, V)>,
    map_one: HashMap<K1, KM>,
    map_two: HashMap<K2, KM>,
}

impl<KM, K1, K2, V> TripleHashMap<KM, K1, K2, V>
where
    KM: Hash + Eq + Clone,
    K1: Hash + Eq + Clone,
    K2: Hash + Eq + Clone,
{
    pub fn new() -> Self {
        TripleHashMap {
            map: HashMap::new(),
            map_one: HashMap::new(),
            map_two: HashMap::new(),
        }
    }

    pub fn get(&self, key: &KM) -> Option<&V> {
        self.map.get(key).map(|(_, v)| v)
    }

    pub fn get_keys(&self, key: &KM) -> Option<&(K1, K2)> {
        self.map.get(key).and_then(|(k, _)| k.as_ref())
    }

    pub fn get_k1(&self, key: &K1) -> Option<(&KM, &V)> {
        self.map_one
            .get(key)
            .and_then(|k| self.map.get(k).map(|(_, v)| (k, v)))
    }

    pub fn get_k2(&self, key: &K2) -> Option<(&KM, &V)> {
        self.map_two
            .get(key)
            .and_then(|k| self.map.get(k).map(|(_, v)| (k, v)))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&KM, &V)> {
        self.map.iter().map(|(k, (_, v))| (k, v))
    }

    pub fn insert(&mut self, k: KM, v: V) {
        self.map.insert(k, (None, v));
    }

    pub fn add_k1_k2(&mut self, k: KM, k1: K1, k2: K2) {
        if let Some((Some((one, two)), _)) = self.map.get_mut(&k) {
            *one = k1.clone();
            *two = k2.clone();

            self.map_one.insert(k1, k.clone());
            self.map_two.insert(k2, k);
        }
    }

    pub fn sub_k1_k2(&mut self, k: &KM) {
        if let Some((keys, _)) = self.map.get_mut(k) {
            if let Some((k1, k2)) = keys.take() {
                self.map_one.remove(&k1);
                self.map_two.remove(&k2);
            }
        }
    }
}

pub struct HashArc<V> {
    inner: Arc<V>,
}

impl<V> Clone for HashArc<V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<V> HashArc<V> {
    pub fn from(arc: Arc<V>) -> Self {
        HashArc { inner: arc }
    }
}

impl<V> PartialEq for HashArc<V> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl<V> Eq for HashArc<V> {}

impl<V> Hash for HashArc<V> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.inner).hash(state)
    }
}
