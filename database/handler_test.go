package database

import (
	"testing"

	"github.com/ag-computational-bio/bakta-web-api-go/api"
)

func TestDatabaseHandler(t *testing.T) {
	databaseHandler, err := InitDatabaseHandler()
	if err != nil {
		t.Errorf(err.Error())
	}

	job1, _, err := databaseHandler.CreateJob(api.RepliconTableType_csv, "test1")
	if err != nil {
		t.Errorf(err.Error())
	}

	job2, _, err := databaseHandler.CreateJob(api.RepliconTableType_csv, "test2")
	if err != nil {
		t.Errorf(err.Error())
	}

	var jobIDs []string
	jobIDs = append(jobIDs, job1.JobID)
	jobIDs = append(jobIDs, job2.JobID)

}

func TestRndBytes(t *testing.T) {
	_, err := randStringBytes(50)
	if err != nil {
		t.Errorf(err.Error())
	}
}
