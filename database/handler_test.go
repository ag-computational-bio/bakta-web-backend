package database

import (
	"testing"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
)

func TestDatabaseHandler(t *testing.T) {
	databaseHandler, err := InitDatabaseHandler()
	if err != nil {
		t.Errorf(err.Error())
	}

	job1, _, err := databaseHandler.CreateJob(api.RepliconTableType_CSV, "test1")
	if err != nil {
		t.Errorf(err.Error())
	}

	job2, _, err := databaseHandler.CreateJob(api.RepliconTableType_CSV, "test2")
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
