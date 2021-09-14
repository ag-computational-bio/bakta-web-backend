// +/build integration

package endpoints

import (
	"context"
	proto_api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/monitor"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"github.com/ag-computational-bio/bakta-web-backend/scheduler"
	"github.com/spf13/viper"
	"testing"
)

func InitAPI() (api *BaktaJobAPI, err error) {
	dbHandler, err := database.InitDatabaseHandler()
	if err != nil {
		return nil, err
	}

	clientset, err := scheduler.CreateClientSet()

	if err != nil {
		return nil, err
	}

	sched, err := scheduler.InitSimpleScheduler(dbHandler, clientset)
	if err != nil {
		return nil, err
	}

	bucket := viper.GetString("Objectstorage.S3.Bucket")

	s3Handler, err := objectStorage.InitS3ObjectStorageHandler(bucket)
	if err != nil {
		return nil, err
	}

	updateMonitor := monitor.New(sched.GetK8sClient(), sched.GetNamespace(), dbHandler)

	return InitBaktaAPI(dbHandler, sched, s3Handler, &updateMonitor), nil

}

func TestBaktaJobAPI_InitJob(t *testing.T) {
	api, err := InitAPI()
	if err != nil {
		t.Fatal(err)
	}
	response, err := api.InitJob(context.Background(), &proto_api.InitJobRequest{
		RepliconTableType: 1,
		Name:              "test",
	})

	if err != nil {
		t.Fatal(err)
	}

	if response != nil {
		t.Fatal("nil response")
	}

}

func TestBaktaJobAPI_StartJob(t *testing.T) {
	api, err := InitAPI()
	if err != nil {

		t.Fatal(err)
	}
	response, err := api.InitJob(context.Background(), &proto_api.InitJobRequest{
		RepliconTableType: 0,
		Name:              "",
	})
	if err != nil {

		t.Fatal(err)
	}

	api.StartJob(context.Background(), &proto_api.StartJobRequest{
		Job: response.Job,
		Config: &proto_api.JobConfig{
			HasProdigal:        false,
			HasReplicons:       false,
			TranslationalTable: 0,
			CompleteGenome:     false,
			KeepContigHeaders:  false,
			MinContigLength:    0,
			DermType:           0,
			Genus:              "",
			Species:            "",
			Strain:             "",
			Plasmid:            "",
			Locus:              "",
			LocusTag:           "",
		},
	})

}

func TestBaktaJobAPI_JobsStatus(t *testing.T) {
	api, err := InitAPI()
	if err != nil {
		t.Fatal(err)
	}
	_, err = api.InitJob(context.Background(), &proto_api.InitJobRequest{
		RepliconTableType: 0,
		Name:              "",
	})
	if err != nil {

		t.Fatal(err)
	}

}
