#![allow(missing_docs)]

use std::{
    collections::{
        hash_map::{Iter, Keys},
        HashMap,
    },
    hash::Hash,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Clone, Copy, Debug)]
pub enum Event<K, V>
where
    K: Clone + Copy,
{
    Added(K),
    Updated(K, V, V),
    Removed(K),
}

type Observer<K, V> = UnboundedSender<Event<K, V>>;

#[derive(Debug)]
pub struct ObservableHashMap<K, V>
where
    K: Clone + Copy,
{
    map: HashMap<K, V>,
    observers: Arc<Mutex<Vec<Observer<K, V>>>>,
}

impl<K, V> ObservableHashMap<K, V>
where
    K: Copy + Clone + Eq + Hash + PartialEq,
    V: Clone + PartialEq,
{
    pub fn new() -> ObservableHashMap<K, V> {
        ObservableHashMap::from(HashMap::new())
    }

    pub fn from(map: HashMap<K, V>) -> ObservableHashMap<K, V> {
        ObservableHashMap {
            map,
            observers: Arc::new(Mutex::new(vec![])),
        }
    }

    fn notify_observers(&self, event: Event<K, V>) {
        let mut observers = self.observers.lock().unwrap();

        // Notify remaining observers
        let mut indices_to_remove = vec![];
        for (idx, tx) in observers.iter().enumerate() {
            if tx.send(event.to_owned()).is_err() {
                // Channel must be closed ¯\_(ツ)_/¯
                indices_to_remove.push(idx);
            }
        }
        for idx in indices_to_remove {
            observers.remove(idx);
        }
    }

    pub fn add_observer(&mut self) -> UnboundedReceiver<Event<K, V>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let mut observers = self.observers.lock().unwrap();
        observers.push(tx);

        rx
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let prev_value = self.map.insert(key, value);

        let event = if let Some(prev_value) = prev_value.as_ref() {
            let value = self.map.get(&key).unwrap();
            Event::Updated(key, prev_value.clone(), value.clone())
        } else {
            Event::Added(key)
        };
        self.notify_observers(event);

        prev_value
    }

    pub fn iter(&self) -> Iter<'_, K, V> {
        self.map.iter()
    }

    pub fn keys(&self) -> Keys<'_, K, V> {
        self.map.keys()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let prev_value = self.map.remove(key);

        if prev_value.is_some() {
            self.notify_observers(Event::Removed(key.to_owned()));
        }

        prev_value
    }

    pub fn update<F>(&mut self, key: &K, mut updater: F) -> bool
    where
        F: FnMut(V) -> V,
    {
        let Some(prev_value) = self.map.get(key) else {
            return false;
        };

        let prev_value = prev_value.to_owned();
        let updated_value = updater(prev_value.clone());
        if updated_value == prev_value {
            return false;
        }

        self.map.insert(*key, updated_value.clone());
        self.notify_observers(Event::Updated(key.to_owned(), prev_value, updated_value));

        true
    }

    pub fn clone(&self) -> ObservableHashMap<K, V> {
        ObservableHashMap::from(self.map.clone())
    }
}

impl<K, V> From<ObservableHashMap<K, V>> for HashMap<K, V>
where
    K: Clone + Copy,
    V: Clone,
{
    fn from(val: ObservableHashMap<K, V>) -> Self {
        val.map
    }
}

impl<K, V> Default for ObservableHashMap<K, V>
where
    K: Clone + Copy,
    V: Clone,
{
    fn default() -> Self {
        Self {
            map: Default::default(),
            observers: Default::default(),
        }
    }
}
