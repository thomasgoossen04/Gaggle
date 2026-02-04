package main

import (
	"encoding/json"

	"github.com/dgraph-io/badger"
)

type User struct {
	ID       string `json:"id"`
	Username string `json:"username"`
	IsAdmin  bool   `json:"is_admin"`
}

type Store struct {
	db *badger.DB
}

func InitDb(path string) (*Store, error) {
	opts := badger.DefaultOptions(path)
	db, err := badger.Open(opts)
	if err != nil {
		return nil, err
	}
	return &Store{db: db}, nil
}

func userKey(id string) []byte {
	return []byte("user:" + id)
}

func (s *Store) UpsertUser(u User) error {
	return s.db.Update(func(txn *badger.Txn) error {
		data, err := json.Marshal(u)
		if err != nil {
			return err
		}
		return txn.Set(userKey(u.ID), data)
	})
}

func (s *Store) GetUser(id string) (*User, error) {
	var user User

	err := s.db.View(func(txn *badger.Txn) error {
		item, err := txn.Get(userKey(id))
		if err != nil {
			return err
		}

		return item.Value(func(val []byte) error {
			return json.Unmarshal(val, &user)
		})
	})

	if err != nil {
		return nil, err
	}

	return &user, nil
}

func (s *Store) DeleteUser(id string) error {
	return s.db.Update(func(txn *badger.Txn) error {
		return txn.Delete(userKey(id))
	})
}

func (s *Store) ListUsers() ([]User, error) {
	users := []User{}

	err := s.db.View(func(txn *badger.Txn) error {
		it := txn.NewIterator(badger.DefaultIteratorOptions)
		defer it.Close()

		prefix := []byte("user:")

		for it.Seek(prefix); it.ValidForPrefix(prefix); it.Next() {
			item := it.Item()

			err := item.Value(func(v []byte) error {
				var u User
				if err := json.Unmarshal(v, &u); err != nil {
					return err
				}
				users = append(users, u)
				return nil
			})
			if err != nil {
				return err
			}
		}

		return nil
	})

	return users, err
}

func (s *Store) CountSessions() (int, error) {
	count := 0
	err := s.db.View(func(txn *badger.Txn) error {
		it := txn.NewIterator(badger.DefaultIteratorOptions)
		defer it.Close()

		prefix := []byte("session:")
		for it.Seek(prefix); it.ValidForPrefix(prefix); it.Next() {
			count++
		}
		return nil
	})
	return count, err
}
