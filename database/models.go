package database

import "time"

// Job The database model for a bakta job
type Job struct {
	JobID       string `gorm:"primaryKey"`
	Secret      string
	K8sID       string `gorm:"index"`
	Updated     int64  `gorm:"autoUpdateTime"` // Use unix milli seconds as updating time
	Created     int64  `gorm:"autoCreateTime"` // Use unix seconds as creating time
	Status      string `gorm:"index"`
	DataBucket  string
	FastaKey    string
	ProdigalKey string
	RepliconKey string
	ResultKey   string
	Error       string
	ExpiryDate  time.Time
}
