package database

import (
	"go.mongodb.org/mongo-driver/bson/primitive"
	"k8s.io/client-go/tools/clientcmd/api"
)

// Job The database model for a bakta job
type Job struct {
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
	Jobname     string
	Config      *api.Config
}
