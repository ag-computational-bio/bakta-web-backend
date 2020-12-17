package database

import (
	"testing"
)

func TestDatabaseHandler(t *testing.T) {
	databaseHandler, err := InitDatabaseHandler(SQLite)
	if err != nil {
		t.Errorf(err.Error())
	}

	job1, _, err := databaseHandler.CreateJob()
	if err != nil {
		t.Errorf(err.Error())
	}

	job2, _, err := databaseHandler.CreateJob()
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
