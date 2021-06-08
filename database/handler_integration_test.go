// +build integration

package database

import (
	"github.com/spf13/viper"
	"os"
	"testing"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
)

//func TestDatabaseHandler(t *testing.T) {
//	databaseHandler, err := InitDatabaseHandler()
//	if err != nil {
//		t.Errorf(err.Error())
//	}
//
//	job1, _, err := databaseHandler.CreateJob(api.RepliconTableType_CSV, "test1")
//	if err != nil {
//		t.Errorf(err.Error())
//	}
//
//	job2, _, err := databaseHandler.CreateJob(api.RepliconTableType_CSV, "test2")
//	if err != nil {
//		t.Errorf(err.Error())
//	}
//
//	var jobIDs []string
//	jobIDs = append(jobIDs, job1.JobID)
//	jobIDs = append(jobIDs, job2.JobID)
//
//}

func TestHandler_CreateJob(t *testing.T) {

	viper.SetConfigFile("../config/config.yaml")
	err := viper.ReadInConfig()
	if err != nil {
		t.Errorf(err.Error())
	}

	db, err := InitDatabaseHandler()
	if err != nil {
		t.Errorf(err.Error())
	}

	job, s, err := db.CreateJob(api.RepliconTableType_TSV, "test")

	if err != nil {
		t.Errorf(err.Error())
	}

	if job == nil || s == "" {
		t.Errorf("empty job response")
	}

}

func TestHandler_GetJob(t *testing.T) {

	os.Setenv("MongoPassword", "testpw1")

	viper.SetConfigFile("../config/config.yaml")
	err := viper.ReadInConfig()
	if err != nil {
		t.Errorf(err.Error())
	}

	db, err := InitDatabaseHandler()
	if err != nil {
		t.Errorf(err.Error())
	}

	job, s, err := db.CreateJob(api.RepliconTableType_TSV, "test2")

	if err != nil {
		t.Errorf(err.Error())
	}

	if job == nil || s == "" {
		t.Errorf("empty job response")
	}

	getJob, err := db.GetJob(job.JobID)

	if err != nil {
		t.Errorf(err.Error())
	}

	if *getJob != *job {
		t.Errorf("created and requested job are not equal")
	}

}

func TestHandler_GetJobs(t *testing.T) {

	//viper.SetConfigFile("../config/config.yaml")
	//err := viper.ReadInConfig()
	//if err != nil {
	//	t.Errorf(err.Error())
	//}
	//
	//db, err := InitDatabaseHandler()
	//if err != nil {
	//	t.Errorf(err.Error())
	//}
	//
	//db.GetJobs(api.JobAuth{
	//	Secret: "",
	//	JobID:  "",
	//})

}

func TestHandler_GetJobStatus(t *testing.T) {

}

func TestRndBytes(t *testing.T) {
	_, err := randStringBytes(50)
	if err != nil {
		t.Errorf(err.Error())
	}
}
