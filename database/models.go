package database

import (
	"go.mongodb.org/mongo-driver/bson/primitive"
)

// Job The database model for a bakta job
type Job struct {
	_id         primitive.ObjectID
	JobID       string
	Secret      string
	K8sID       string
	Updated     primitive.Timestamp
	Created     primitive.Timestamp
	Status      string
	DataBucket  string
	FastaKey    string
	ProdigalKey string
	RepliconKey string
	ResultKey   string
	Error       string
	ExpiryDate  primitive.Timestamp
	ConfString  string
	IsDeleted   bool
}
