package main

import (
	"encoding/json"
	"time"

	"github.com/dgraph-io/badger"
)

type PlaytimeEntry struct {
	AppID        string `json:"app_id"`
	TotalSeconds int64  `json:"total_seconds"`
	LastPlayed   int64  `json:"last_played"`
}

func playtimeKey(userID, appID string) []byte {
	return []byte("playtime:" + userID + ":" + appID)
}

func playtimePrefix(userID string) []byte {
	return []byte("playtime:" + userID + ":")
}

func (s *Store) GetPlaytime(userID, appID string) (*PlaytimeEntry, error) {
	var entry PlaytimeEntry
	err := s.db.View(func(txn *badger.Txn) error {
		item, err := txn.Get(playtimeKey(userID, appID))
		if err != nil {
			return err
		}
		return item.Value(func(val []byte) error {
			return json.Unmarshal(val, &entry)
		})
	})
	if err != nil {
		return nil, err
	}
	return &entry, nil
}

func (s *Store) AddPlaytime(userID, appID string, seconds int64, endedAt time.Time) (*PlaytimeEntry, error) {
	if seconds <= 0 {
		seconds = 0
	}
	next := PlaytimeEntry{
		AppID:        appID,
		TotalSeconds: seconds,
		LastPlayed:   endedAt.Unix(),
	}

	err := s.db.Update(func(txn *badger.Txn) error {
		item, err := txn.Get(playtimeKey(userID, appID))
		if err == nil {
			var current PlaytimeEntry
			if err := item.Value(func(val []byte) error {
				return json.Unmarshal(val, &current)
			}); err == nil {
				next.TotalSeconds = current.TotalSeconds + seconds
				if endedAt.Unix() > current.LastPlayed {
					next.LastPlayed = endedAt.Unix()
				} else {
					next.LastPlayed = current.LastPlayed
				}
			}
		} else if err != badger.ErrKeyNotFound {
			return err
		}

		data, err := json.Marshal(next)
		if err != nil {
			return err
		}
		return txn.Set(playtimeKey(userID, appID), data)
	})
	if err != nil {
		return nil, err
	}
	return &next, nil
}

func (s *Store) ListPlaytime(userID string) ([]PlaytimeEntry, error) {
	results := []PlaytimeEntry{}
	err := s.db.View(func(txn *badger.Txn) error {
		it := txn.NewIterator(badger.DefaultIteratorOptions)
		defer it.Close()

		prefix := playtimePrefix(userID)
		for it.Seek(prefix); it.ValidForPrefix(prefix); it.Next() {
			item := it.Item()
			if err := item.Value(func(val []byte) error {
				var entry PlaytimeEntry
				if err := json.Unmarshal(val, &entry); err != nil {
					return err
				}
				results = append(results, entry)
				return nil
			}); err != nil {
				return err
			}
		}
		return nil
	})
	return results, err
}
