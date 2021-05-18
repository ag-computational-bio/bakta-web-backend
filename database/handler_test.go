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

	job1, _, err := databaseHandler.CreateJob(api.RepliconTableType_csv)
	if err != nil {
		t.Errorf(err.Error())
	}

	job2, _, err := databaseHandler.CreateJob(api.RepliconTableType_csv)
	if err != nil {
		t.Errorf(err.Error())
	}

	var jobIDs []string
	jobIDs = append(jobIDs, job1.JobID)
	jobIDs = append(jobIDs, job2.JobID)

	_, err = databaseHandler.GetJobsStatus(jobIDs)
	if err != nil {
		t.Errorf(err.Error())
	}
}

func TestRndBytes(t *testing.T) {
	_, err := randStringBytes(50)
	if err != nil {
		t.Errorf(err.Error())
	}
}
