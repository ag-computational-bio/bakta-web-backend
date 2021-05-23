package database

import (
	"time"

	"go.mongodb.org/mongo-driver/bson/primitive"
)

// Job The database model for a bakta job
type Job struct {
	_id         primitive.ObjectID
	JobID       string
	Secret      string
	K8sID       string
	Updated     time.Time
	Created     time.Time
	Status      string
	DataBucket  string
	FastaKey    string
	ProdigalKey string
	RepliconKey string
	ResultKey   string
	Error       string
	ConfString  string
	IsDeleted   bool
}
