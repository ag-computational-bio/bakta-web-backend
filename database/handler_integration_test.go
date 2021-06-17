// +build integration

package database

import (
	"fmt"
	"github.com/spf13/viper"
	"os"
	"testing"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
)

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

func TestHandler_UpdateStatus(t *testing.T) {

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

	err = db.UpdateStatus(job.JobID, api.JobStatusEnum_RUNNING, "", false)

	if err != nil {
		t.Errorf(err.Error())
	}

	ret_job, err := db.GetJobStatus(job.JobID)

	if err != nil {
		t.Errorf(err.Error())
	}

	fmt.Println(ret_job.Status)
	if ret_job.Status != "RUNNING" {
		t.Errorf("Status update failed")
	}

}

func TestRndBytes(t *testing.T) {
	_, err := randStringBytes(50)
	if err != nil {
		t.Errorf(err.Error())
	}
}
